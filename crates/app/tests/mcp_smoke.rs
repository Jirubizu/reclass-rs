//! End-to-end smoke test of the MCP server over a real loopback socket.
//!
//! Starts the server, runs a drainer thread that plays the GUI's role
//! (applying forwarded ops to a live `AppState`), then speaks JSON-RPC over TCP
//! exactly as an MCP client (e.g. IDA Pro MCP's agent) would.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reclass::app_state::AppState;
use reclass::mcp;

/// Send one JSON-RPC message and return the parsed response body.
fn rpc(port: u16, body: &str) -> serde_json::Value {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let req = format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let payload = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or_default();
    serde_json::from_str(payload).unwrap_or_else(|e| panic!("bad JSON body: {e}\n---\n{text}"))
}

#[test]
fn drives_reclass_over_http() {
    // Pick a fixed loopback port; skip cleanly if it is already taken so the
    // test never flakes a build.
    let port = 39017;
    let stop = Arc::new(AtomicBool::new(false));
    let rt = match mcp::start(port, || {}) {
        Ok(rt) => rt,
        Err(_) => return, // port busy → skip
    };

    // Drainer thread: the GUI's job — apply forwarded ops to live state.
    let stop_d = stop.clone();
    let drainer = std::thread::spawn(move || {
        let mut state = AppState::new();
        while !stop_d.load(Ordering::SeqCst) {
            let mut idle = true;
            while let Some(call) = rt.try_recv() {
                idle = false;
                let result = mcp::dispatch(&mut state, &call.op);
                let _ = call.reply.send(result);
            }
            if idle {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        rt.stop();
    });

    // initialize
    let init = rpc(
        port,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "reclass-rs");

    // tools/list carries the catalog
    let tools = rpc(port, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"create_class"));
    assert!(names.contains(&"add_node"));

    // create a class via tools/call
    let created = rpc(
        port,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_class","arguments":{"name":"Enemy"}}}"#,
    );
    assert_eq!(created["result"]["isError"], false);

    // add a field, then read the class back — proves the write reached AppState
    let _ = rpc(
        port,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"add_node","arguments":{"class_id":0,"kind":"f32","name":"health"}}}"#,
    );
    let class = rpc(
        port,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_class","arguments":{"id":0}}}"#,
    );
    let text = class["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["name"], "Enemy");
    assert_eq!(parsed["nodes"][0]["name"], "health");

    // unknown method → JSON-RPC error object
    let err = rpc(
        port,
        r#"{"jsonrpc":"2.0","id":6,"method":"does/not/exist"}"#,
    );
    assert_eq!(err["error"]["code"], -32601);

    stop.store(true, Ordering::SeqCst);
    drainer.join().unwrap();
}
