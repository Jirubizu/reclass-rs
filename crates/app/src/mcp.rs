//! In-process MCP (Model Context Protocol) server exposing full control over
//! the live reclass-rs state to MCP clients — e.g. pairing with IDA Pro MCP so
//! an agent can read code in IDA and build the matching structures here.
//!
//! ## Transport
//! Streamable-HTTP-compatible JSON-RPC 2.0 over a **loopback** TCP socket
//! (`tiny_http`, synchronous). Bound to `127.0.0.1` only: the tools read and
//! write live process memory, so the surface is never exposed off-host.
//! `POST` carries a single JSON-RPC message; `GET` (the SSE upgrade path) is
//! answered `405` since the server never initiates messages — clients see live
//! state by polling, and the human sees it in the GUI.
//!
//! ## Threading
//! The server runs on its own thread and **never touches [`AppState`]**.
//! Sharing `AppState` would force `Send` bounds through the whole memory
//! backend; instead every request that needs live state is forwarded over an
//! mpsc channel to the GUI thread (the sole owner of `AppState`), which applies
//! it via [`dispatch`] and replies. That is exactly why MCP writes show up
//! live: the GUI mutates its own state and repaints on the next tick.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use reclass_backend_vmem::{VmemBackend, list_processes, process_name};
use reclass_core::codegen::Language;
use reclass_core::{IntWidth, Node, NodeKind};
use serde_json::{Value, json};

use crate::app_state::AppState;

/// How long a forwarded request waits for the GUI to answer before giving up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Accept/reply poll granularity — also the max shutdown latency.
const POLL: Duration = Duration::from_millis(100);
/// Hard cap on a single `read_memory` request.
const MAX_READ: usize = 1 << 20;

// ---------------------------------------------------------------------------
// GUI-facing handle
// ---------------------------------------------------------------------------

/// A request forwarded from the server thread to the GUI thread.
pub struct Call {
    /// The operation to perform against live state.
    pub op: Op,
    /// Where to send the result.
    pub reply: Sender<Result<Value, String>>,
}

/// What a forwarded [`Call`] wants done.
pub enum Op {
    /// A `tools/call`: run tool `name` with JSON `args`.
    Tool { name: String, args: Value },
    /// A `resources/read`: read resource `uri`.
    Resource { uri: String },
}

/// A running MCP server. Dropping it (or calling [`stop`](Self::stop)) signals
/// the server thread to exit and joins it.
pub struct McpRuntime {
    port: u16,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    rx: Receiver<Call>,
}

impl McpRuntime {
    /// The port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Take the next pending request from the server thread, if any. Called by
    /// the GUI once per frame; each returned [`Call`] must be answered via its
    /// `reply` channel.
    pub fn try_recv(&self) -> Option<Call> {
        self.rx.try_recv().ok()
    }

    /// Stop the server and join its thread.
    pub fn stop(mut self) {
        self.shutdown_and_join();
    }

    fn shutdown_and_join(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for McpRuntime {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

/// Start an MCP server on `127.0.0.1:port`. `wake` is invoked whenever a
/// request is forwarded, so the GUI can `request_repaint` and answer promptly.
pub fn start<W>(port: u16, wake: W) -> Result<McpRuntime, String>
where
    W: Fn() + Send + 'static,
{
    let server = tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    let (tx, rx) = channel::<Call>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let sh = shutdown.clone();
    let join = std::thread::Builder::new()
        .name("reclass-mcp".into())
        .spawn(move || serve(server, &tx, &wake, &sh))
        .map_err(|e| e.to_string())?;
    Ok(McpRuntime {
        port,
        shutdown,
        join: Some(join),
        rx,
    })
}

// ---------------------------------------------------------------------------
// server thread
// ---------------------------------------------------------------------------

fn serve(server: tiny_http::Server, tx: &Sender<Call>, wake: &impl Fn(), shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::SeqCst) {
        let mut req = match server.recv_timeout(POLL) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(_) => break,
        };
        if *req.method() != tiny_http::Method::Post {
            let _ = req.respond(text_response(405, "POST JSON-RPC to this endpoint"));
            continue;
        }
        let mut body = String::new();
        if req.as_reader().read_to_string(&mut body).is_err() {
            let _ = req.respond(json_response(&rpc_error(
                Value::Null,
                -32700,
                "body not UTF-8",
            )));
            continue;
        }
        match handle_rpc(&body, tx, wake, shutdown) {
            Some(resp) => {
                let _ = req.respond(json_response(&resp));
            }
            // A notification (no `id`) gets an empty 202 with no JSON-RPC body.
            None => {
                let _ = req.respond(text_response(202, ""));
            }
        }
    }
}

/// Route one JSON-RPC message. Returns the response body, or `None` for a
/// notification (no `id` → no reply, per JSON-RPC 2.0).
fn handle_rpc(
    body: &str,
    tx: &Sender<Call>,
    wake: &impl Fn(),
    shutdown: &AtomicBool,
) -> Option<String> {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let id = v.get("id").cloned();
    let method = v.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    let result = route(method, &params, tx, wake, shutdown);
    // notifications carry no id and expect no response
    let id = id?;
    Some(match result {
        Ok(r) => rpc_result(id, r),
        Err(e) => rpc_error(id, e.0, &e.1),
    })
}

/// `(code, message)` for a JSON-RPC error.
type RpcErr = (i64, String);

fn route(
    method: &str,
    params: &Value,
    tx: &Sender<Call>,
    wake: &impl Fn(),
    shutdown: &AtomicBool,
) -> Result<Value, RpcErr> {
    match method {
        "initialize" => Ok(initialize_result(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_catalog() })),
        "resources/list" => Ok(json!({ "resources": resource_catalog() })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing tool name".to_string()))?;
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let op = Op::Tool {
                name: name.to_string(),
                args,
            };
            Ok(match forward(op, tx, wake, shutdown) {
                Ok(v) => json!({ "content": [text_content(&v)], "isError": false }),
                Err(msg) => {
                    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
                }
            })
        }
        "resources/read" => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing resource uri".to_string()))?
                .to_string();
            let op = Op::Resource { uri: uri.clone() };
            match forward(op, tx, wake, shutdown) {
                Ok(v) => Ok(json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": to_text(&v),
                    }]
                })),
                Err(msg) => Err((-32603, msg)),
            }
        }
        m if m.starts_with("notifications/") => Ok(Value::Null),
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn initialize_result(params: &Value) -> Value {
    // Echo the client's protocol version when present (maximally compatible),
    // else advertise the version this was written against.
    let ver = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-06-18");
    json!({
        "protocolVersion": ver,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "listChanged": false, "subscribe": false },
        },
        "serverInfo": { "name": "reclass-rs", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// Forward an op to the GUI thread and block (bounded) on its reply. Honors the
/// shutdown flag so a stop never waits the full timeout.
fn forward(
    op: Op,
    tx: &Sender<Call>,
    wake: &impl Fn(),
    shutdown: &AtomicBool,
) -> Result<Value, String> {
    let (rtx, rrx) = channel();
    if tx.send(Call { op, reply: rtx }).is_err() {
        return Err("gui thread unavailable".to_string());
    }
    wake();
    let deadline = std::time::Instant::now() + REPLY_TIMEOUT;
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Err("server shutting down".to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err("timed out waiting for gui".to_string());
        }
        match rrx.recv_timeout(POLL) {
            Ok(r) => return r,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err("gui dropped request".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP / JSON-RPC helpers
// ---------------------------------------------------------------------------

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("static header is valid")
}

fn json_response(body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body).with_header(header("Content-Type", "application/json"))
}

fn text_response(code: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(body)
        .with_status_code(code)
        .with_header(header("Content-Type", "text/plain"))
}

fn rpc_result(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn rpc_error(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn text_content(v: &Value) -> Value {
    json!({ "type": "text", "text": to_text(v) })
}

fn to_text(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

// ---------------------------------------------------------------------------
// tool + resource catalogs
// ---------------------------------------------------------------------------

/// Kind-argument documentation shared by every tool that takes one.
const KIND_DOC: &str = "Field type. Shorthand string: u8/u16/u32/u64, i8/i16/i32/i64, \
f32, f64, bool, ptr, fnptr, hex8/hex16/hex32/hex64 (hex=hex64), vec2/vec3/vec4. \
Or a full NodeKind JSON object for complex types, e.g. {\"Array\":{\"element\":{\"Hex\":\"W64\"},\"count\":8}}, \
{\"ClassPtr\":{\"class_id\":3}}, {\"ClassInstance\":{\"class_id\":3}}, {\"Text\":{\"encoding\":\"Utf8\",\"len\":32}}, {\"Padding\":16}.";

/// `(name, description, inputSchema JSON)` for every tool. The catalog and
/// [`dispatch`] share these names; a test asserts every name is handled.
fn tool_defs() -> Vec<(&'static str, String, Value)> {
    let obj = |props: Value, required: Value| json!({ "type": "object", "properties": props, "required": required });
    vec![
        ("list_classes", "List every class: id, name, byte size, field count.".into(), obj(json!({}), json!([]))),
        ("get_class", "Full definition of one class: fields with index, name, kind, offset, comment.".into(),
            obj(json!({ "id": { "type": "integer" } }), json!(["id"]))),
        ("create_class", "Create a new empty class and open it in a view. Returns its id.".into(),
            obj(json!({ "name": { "type": "string" } }), json!([]))),
        ("remove_class", "Delete a class by id.".into(),
            obj(json!({ "id": { "type": "integer" } }), json!(["id"]))),
        ("rename_class", "Rename a class.".into(),
            obj(json!({ "id": { "type": "integer" }, "name": { "type": "string" } }), json!(["id", "name"]))),
        ("set_address_expr", "Set a class's address-bar expression (e.g. \"game.exe+0x1234\").".into(),
            obj(json!({ "id": { "type": "integer" }, "expr": { "type": "string" } }), json!(["id", "expr"]))),
        ("add_node", format!("Append a field to a class. {KIND_DOC}"),
            obj(json!({ "class_id": { "type": "integer" }, "kind": {}, "name": { "type": "string" }, "comment": { "type": "string" } }), json!(["class_id", "kind"]))),
        ("insert_node", format!("Insert a field at `index` (shifts later fields). {KIND_DOC}"),
            obj(json!({ "class_id": { "type": "integer" }, "index": { "type": "integer" }, "kind": {}, "name": { "type": "string" } }), json!(["class_id", "index", "kind"]))),
        ("remove_node", "Delete field `index` from a class.".into(),
            obj(json!({ "class_id": { "type": "integer" }, "index": { "type": "integer" } }), json!(["class_id", "index"]))),
        ("set_node_kind", format!("Change field `index`'s type. {KIND_DOC}"),
            obj(json!({ "class_id": { "type": "integer" }, "index": { "type": "integer" }, "kind": {} }), json!(["class_id", "index", "kind"]))),
        ("set_node_name", "Rename field `index`.".into(),
            obj(json!({ "class_id": { "type": "integer" }, "index": { "type": "integer" }, "name": { "type": "string" } }), json!(["class_id", "index", "name"]))),
        ("set_node_comment", "Set field `index`'s comment.".into(),
            obj(json!({ "class_id": { "type": "integer" }, "index": { "type": "integer" }, "comment": { "type": "string" } }), json!(["class_id", "index", "comment"]))),
        ("set_array_count", "Set the element count of an Array field.".into(),
            obj(json!({ "class_id": { "type": "integer" }, "index": { "type": "integer" }, "count": { "type": "integer" } }), json!(["class_id", "index", "count"]))),
        ("add_bytes", "Append `count` bytes of fields (hex64 rows + hex8 remainder) to grow a class in bulk.".into(),
            obj(json!({ "class_id": { "type": "integer" }, "count": { "type": "integer" } }), json!(["class_id", "count"]))),
        ("read_memory", "Read raw bytes from the attached target. Returns lowercase hex. `address` may be a number or 0x-string.".into(),
            obj(json!({ "address": {}, "size": { "type": "integer" } }), json!(["address", "size"]))),
        ("write_memory", "Write raw bytes to the attached target. Provide `hex` (string) or `bytes` (array of u8).".into(),
            obj(json!({ "address": {}, "hex": { "type": "string" }, "bytes": { "type": "array" } }), json!(["address"]))),
        ("list_regions", "List mapped memory regions of the attached target (start, end, perms, path).".into(),
            obj(json!({}), json!([]))),
        ("list_processes", "List processes visible for attaching (pid, name).".into(),
            obj(json!({}), json!([]))),
        ("attach_pid", "Attach reclass to a process by pid.".into(),
            obj(json!({ "pid": { "type": "integer" } }), json!(["pid"]))),
        ("get_rows", "Snapshot of the currently rendered rows across all views, with live values.".into(),
            obj(json!({}), json!([]))),
        ("codegen", "Generate source for every class. `lang` is one of: rust, c, cpp.".into(),
            obj(json!({ "lang": { "type": "string" } }), json!(["lang"]))),
        ("save_project", "Save the project to a .ron file.".into(),
            obj(json!({ "path": { "type": "string" } }), json!(["path"]))),
        ("load_project", "Load a project from a .ron file (replaces current state).".into(),
            obj(json!({ "path": { "type": "string" } }), json!(["path"]))),
    ]
}

fn tool_catalog() -> Value {
    Value::Array(
        tool_defs()
            .into_iter()
            .map(|(name, description, schema)| {
                json!({ "name": name, "description": description, "inputSchema": schema })
            })
            .collect(),
    )
}

fn resource_catalog() -> Value {
    json!([
        { "uri": "reclass://classes", "name": "Classes", "description": "All classes and their fields.", "mimeType": "application/json" },
        { "uri": "reclass://regions", "name": "Memory regions", "description": "Mapped regions of the attached target.", "mimeType": "application/json" },
        { "uri": "reclass://rows", "name": "Live rows", "description": "Currently rendered rows with live values.", "mimeType": "application/json" },
        { "uri": "reclass://codegen/rust", "name": "Generated Rust", "description": "Rust struct definitions.", "mimeType": "text/plain" },
        { "uri": "reclass://codegen/cpp", "name": "Generated C++", "description": "C++ struct definitions.", "mimeType": "text/plain" },
    ])
}

// ---------------------------------------------------------------------------
// dispatch — runs on the GUI thread with live &mut AppState
// ---------------------------------------------------------------------------

/// Execute a forwarded [`Op`] against live state. Runs on the GUI thread, so it
/// has full mutable access to [`AppState`] and its memory backend.
pub fn dispatch(state: &mut AppState, op: &Op) -> Result<Value, String> {
    match op {
        Op::Tool { name, args } => tool(state, name, args),
        Op::Resource { uri } => resource(state, uri),
    }
}

fn tool(state: &mut AppState, name: &str, a: &Value) -> Result<Value, String> {
    match name {
        "list_classes" => Ok(json!({ "classes": classes_json(state) })),
        "get_class" => {
            class_json(state, arg_u32(a, "id")?).ok_or_else(|| "no such class".to_string())
        }
        "create_class" => {
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            let id = match name {
                Some(n) => state.add_class(n),
                None => {
                    let n = format!("Class{}", state.project.registry.len() + 1);
                    state.add_class(n)
                }
            };
            Ok(json!({ "id": id }))
        }
        "remove_class" => {
            state.remove_class(arg_u32(a, "id")?);
            Ok(json!({ "ok": true }))
        }
        "rename_class" => {
            state.rename_class(arg_u32(a, "id")?, arg_str(a, "name")?.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "set_address_expr" => {
            state.set_address_expr(arg_u32(a, "id")?, arg_str(a, "expr")?.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "add_node" => {
            let cid = arg_u32(a, "class_id")?;
            let kind = parse_kind(a.get("kind").ok_or("missing 'kind'")?)?;
            guard_cycle(state, cid, &kind)?;
            let idx = class_len(state, cid)?;
            let off = state.project.registry.size_of(cid);
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .map_or_else(|| format!("field_{off:X}"), str::to_string);
            let mut node = Node::new(name, kind);
            if let Some(c) = a.get("comment").and_then(Value::as_str) {
                node.comment = c.to_string();
            }
            state.push_node(cid, node)?;
            Ok(json!({ "index": idx }))
        }
        "insert_node" => {
            let cid = arg_u32(a, "class_id")?;
            let index = arg_usize(a, "index")?;
            let kind = parse_kind(a.get("kind").ok_or("missing 'kind'")?)?;
            guard_cycle(state, cid, &kind)?;
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .map_or_else(|| format!("field_{index}"), str::to_string);
            state
                .project
                .registry
                .insert_node(cid, index, Node::new(name, kind))
                .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "remove_node" => {
            state.delete_node(arg_u32(a, "class_id")?, arg_usize(a, "index")?)?;
            Ok(json!({ "ok": true }))
        }
        "set_node_kind" => {
            let kind = parse_kind(a.get("kind").ok_or("missing 'kind'")?)?;
            state.change_kind(arg_u32(a, "class_id")?, arg_usize(a, "index")?, kind)?;
            Ok(json!({ "ok": true }))
        }
        "set_node_name" => {
            state.rename_node(
                arg_u32(a, "class_id")?,
                arg_usize(a, "index")?,
                arg_str(a, "name")?.to_string(),
            )?;
            Ok(json!({ "ok": true }))
        }
        "set_node_comment" => {
            state.set_comment(
                arg_u32(a, "class_id")?,
                arg_usize(a, "index")?,
                arg_str(a, "comment")?.to_string(),
            )?;
            Ok(json!({ "ok": true }))
        }
        "set_array_count" => {
            state.set_array_count(
                arg_u32(a, "class_id")?,
                arg_usize(a, "index")?,
                arg_usize(a, "count")?,
            )?;
            Ok(json!({ "ok": true }))
        }
        "add_bytes" => {
            state.add_bytes(arg_u32(a, "class_id")?, arg_usize(a, "count")?)?;
            Ok(json!({ "ok": true }))
        }
        "read_memory" => {
            let addr = arg_u64(a, "address")?;
            let size = arg_usize(a, "size")?.min(MAX_READ);
            let backend = state.backend.as_ref().ok_or("not attached")?;
            let mut buf = vec![0u8; size];
            backend.read(addr, &mut buf).map_err(|e| e.to_string())?;
            Ok(json!({ "address": addr, "size": size, "hex": hex_encode(&buf) }))
        }
        "write_memory" => {
            let addr = arg_u64(a, "address")?;
            let bytes = write_bytes(a)?;
            let backend = state.backend.as_ref().ok_or("not attached")?;
            backend.write(addr, &bytes).map_err(|e| e.to_string())?;
            Ok(json!({ "written": bytes.len() }))
        }
        "list_regions" => {
            state.refresh_regions();
            Ok(json!({ "regions": regions_json(state) }))
        }
        "list_processes" => Ok(json!({
            "processes": list_processes()
                .into_iter()
                .map(|p| json!({ "pid": p.pid, "name": p.name }))
                .collect::<Vec<_>>()
        })),
        "attach_pid" => {
            let pid = arg_i32(a, "pid")?;
            let backend = VmemBackend::by_pid(pid).map_err(|e| e.to_string())?;
            state.set_backend(Box::new(backend));
            state.project.attach_name = process_name(pid);
            state.status = format!("attached to pid {pid} (via MCP)");
            Ok(json!({ "pid": pid, "name": state.project.attach_name }))
        }
        "get_rows" => Ok(json!({ "rows": rows_json(state) })),
        "codegen" => {
            let lang = parse_lang(arg_str(a, "lang")?)?;
            Ok(json!({ "lang": arg_str(a, "lang")?, "source": state.codegen(lang) }))
        }
        "save_project" => {
            state.save(arg_str(a, "path")?)?;
            Ok(json!({ "ok": true }))
        }
        "load_project" => {
            state.load(arg_str(a, "path")?)?;
            Ok(json!({ "ok": true }))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn resource(state: &mut AppState, uri: &str) -> Result<Value, String> {
    match uri {
        "reclass://classes" => Ok(json!({ "classes": classes_json(state) })),
        "reclass://regions" => {
            state.refresh_regions();
            Ok(json!({ "regions": regions_json(state) }))
        }
        "reclass://rows" => Ok(json!({ "rows": rows_json(state) })),
        "reclass://codegen/rust" => Ok(json!({ "source": state.codegen(Language::Rust) })),
        "reclass://codegen/cpp" => Ok(json!({ "source": state.codegen(Language::Cpp) })),
        other => Err(format!("unknown resource: {other}")),
    }
}

// ---------------------------------------------------------------------------
// state → JSON
// ---------------------------------------------------------------------------

fn classes_json(state: &AppState) -> Vec<Value> {
    let reg = &state.project.registry;
    reg.ids()
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "name": reg.name_of(id).unwrap_or(""),
                "size": reg.size_of(id),
                "fields": reg.get(id).map_or(0, |c| c.nodes.len()),
            })
        })
        .collect()
}

fn class_json(state: &AppState, id: u32) -> Option<Value> {
    let reg = &state.project.registry;
    let class = reg.get(id)?;
    let offs = reg.offsets(id);
    let nodes: Vec<Value> = class
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            json!({
                "index": i,
                "name": n.name,
                "comment": n.comment,
                "kind": n.kind,
                "offset": offs.get(i).copied().unwrap_or(0),
            })
        })
        .collect();
    Some(json!({
        "id": id,
        "name": class.name,
        "address_expr": class.address_expr,
        "size": reg.size_of(id),
        "nodes": nodes,
    }))
}

fn regions_json(state: &AppState) -> Vec<Value> {
    state
        .regions
        .iter()
        .map(|r| {
            json!({
                "start": r.start,
                "end": r.end,
                "perms": r.perms.to_string(),
                "path": r.path,
            })
        })
        .collect()
}

fn rows_json(state: &mut AppState) -> Vec<Value> {
    state
        .compute_rows()
        .iter()
        .map(|r| {
            json!({
                "depth": r.depth,
                "root": r.root,
                "offset": r.offset,
                "address": r.address,
                "type": r.type_label,
                "name": r.name,
                "value": r.value,
                "hex": r.hex,
                "comment": r.comment,
                "readable": r.readable,
                "expandable": r.expandable,
                "expanded": r.expanded,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// argument parsing
// ---------------------------------------------------------------------------

fn guard_cycle(state: &AppState, class: u32, kind: &NodeKind) -> Result<(), String> {
    if state.project.registry.kind_would_cycle(class, kind) {
        return Err("would create an inline class cycle".to_string());
    }
    Ok(())
}

fn class_len(state: &AppState, class: u32) -> Result<usize, String> {
    state
        .project
        .registry
        .get(class)
        .map(|c| c.nodes.len())
        .ok_or_else(|| "no such class".to_string())
}

fn arg_str<'a>(a: &'a Value, key: &str) -> Result<&'a str, String> {
    a.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string '{key}'"))
}

fn arg_u64(a: &Value, key: &str) -> Result<u64, String> {
    let v = a.get(key).ok_or_else(|| format!("missing '{key}'"))?;
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    if let Some(s) = v.as_str() {
        return parse_u64(s);
    }
    Err(format!("'{key}' must be a number or numeric string"))
}

fn arg_u32(a: &Value, key: &str) -> Result<u32, String> {
    u32::try_from(arg_u64(a, key)?).map_err(|_| format!("'{key}' out of range for u32"))
}

fn arg_usize(a: &Value, key: &str) -> Result<usize, String> {
    usize::try_from(arg_u64(a, key)?).map_err(|_| format!("'{key}' out of range"))
}

fn arg_i32(a: &Value, key: &str) -> Result<i32, String> {
    let v = a.get(key).ok_or_else(|| format!("missing '{key}'"))?;
    if let Some(n) = v.as_i64() {
        return i32::try_from(n).map_err(|_| format!("'{key}' out of range for i32"));
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<i32>().map_err(|e| e.to_string());
    }
    Err(format!("'{key}' must be an integer"))
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).map_err(|e| e.to_string()),
        None => s.parse::<u64>().map_err(|e| e.to_string()),
    }
}

fn parse_lang(s: &str) -> Result<Language, String> {
    match s.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Ok(Language::Rust),
        "c" => Ok(Language::C),
        "cpp" | "c++" | "cxx" => Ok(Language::Cpp),
        _ => Err(format!("unknown language '{s}' (use rust, c, or cpp)")),
    }
}

/// Parse the `kind` argument: a shorthand string for common scalars, or a full
/// serde NodeKind JSON object for complex types (Array, ClassPtr, Text, …).
fn parse_kind(v: &Value) -> Result<NodeKind, String> {
    use IntWidth::{W8, W16, W32, W64};
    if let Some(s) = v.as_str() {
        let k = match s.to_ascii_lowercase().as_str() {
            "u8" => NodeKind::UInt(W8),
            "u16" => NodeKind::UInt(W16),
            "u32" => NodeKind::UInt(W32),
            "u64" => NodeKind::UInt(W64),
            "i8" => NodeKind::Int(W8),
            "i16" => NodeKind::Int(W16),
            "i32" => NodeKind::Int(W32),
            "i64" => NodeKind::Int(W64),
            "f32" => NodeKind::Float32,
            "f64" => NodeKind::Float64,
            "bool" => NodeKind::Bool,
            "ptr" | "pointer" => NodeKind::Pointer,
            "fnptr" | "functionptr" => NodeKind::FunctionPtr,
            "hex8" => NodeKind::Hex(W8),
            "hex16" => NodeKind::Hex(W16),
            "hex32" => NodeKind::Hex(W32),
            "hex64" | "hex" => NodeKind::Hex(W64),
            "vec2" => NodeKind::Vec2,
            "vec3" => NodeKind::Vec3,
            "vec4" => NodeKind::Vec4,
            other => return Err(format!("unknown kind shorthand '{other}'")),
        };
        Ok(k)
    } else {
        serde_json::from_value::<NodeKind>(v.clone())
            .map_err(|e| format!("invalid kind object: {e}"))
    }
}

fn write_bytes(a: &Value) -> Result<Vec<u8>, String> {
    if let Some(h) = a.get("hex").and_then(Value::as_str) {
        return hex_decode(h);
    }
    if let Some(arr) = a.get("bytes").and_then(Value::as_array) {
        return arr
            .iter()
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n <= u64::from(u8::MAX))
                    .map(|n| n as u8)
                    .ok_or_else(|| "bytes must be integers 0..=255".to_string())
            })
            .collect();
    }
    Err("provide 'hex' (string) or 'bytes' (array)".to_string())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if !s.len().is_multiple_of(2) {
        return Err("hex string has odd length".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_tool_is_handled() {
        // An empty-args call to each tool must not hit the unknown-tool arm.
        let mut state = AppState::new();
        for (name, _, _) in tool_defs() {
            let err = tool(&mut state, name, &json!({})).err().unwrap_or_default();
            assert_ne!(
                err,
                format!("unknown tool: {name}"),
                "tool `{name}` has no handler"
            );
        }
    }

    #[test]
    fn create_add_and_read_back_a_class() {
        let mut state = AppState::new();
        let created = tool(&mut state, "create_class", &json!({ "name": "Player" })).unwrap();
        let id = created["id"].as_u64().unwrap();

        tool(
            &mut state,
            "add_node",
            &json!({ "class_id": id, "kind": "f32", "name": "health" }),
        )
        .unwrap();
        tool(
            &mut state,
            "add_node",
            &json!({ "class_id": id, "kind": "u32" }),
        )
        .unwrap();
        // complex kind via serde object
        tool(&mut state, "add_node", &json!({ "class_id": id, "kind": { "Array": { "element": { "Hex": "W64" }, "count": 4 } } })).unwrap();

        let class = tool(&mut state, "get_class", &json!({ "id": id })).unwrap();
        let nodes = class["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0]["name"], "health");
        assert_eq!(nodes[0]["offset"], 0);
        assert_eq!(nodes[1]["offset"], 4); // after the f32
        assert_eq!(class["name"], "Player");
    }

    #[test]
    fn kind_shorthand_and_errors() {
        assert_eq!(
            parse_kind(&json!("u64")).unwrap(),
            NodeKind::UInt(IntWidth::W64)
        );
        assert_eq!(parse_kind(&json!("ptr")).unwrap(), NodeKind::Pointer);
        assert!(parse_kind(&json!("nonsense")).is_err());
        assert_eq!(parse_u64("0x10").unwrap(), 16);
        assert_eq!(parse_u64("42").unwrap(), 42);
        assert_eq!(hex_encode(&[0xde, 0xad]), "dead");
        assert_eq!(hex_decode("dead").unwrap(), vec![0xde, 0xad]);
        assert!(hex_decode("abc").is_err());
    }
}
