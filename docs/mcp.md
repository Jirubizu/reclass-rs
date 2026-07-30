# MCP server

reclass-rs can expose its live state to an AI agent over the
[Model Context Protocol](https://modelcontextprotocol.io), so a tool like
[IDA Pro MCP](https://github.com/mrexodia/ida-pro-mcp) can drive it: the agent
reads code / xrefs / decompilation in IDA and **builds the matching structures
here**, with fields and offsets appearing in the window as it works.

- **Transport:** JSON-RPC 2.0 over HTTP (POST; streamable-HTTP compatible), bound
  to **`127.0.0.1` only** — never exposed off-host.
- **Off by default.** Enable it in *View → Settings → **MCP control server***:
  tick **enabled** and pick a **port** (default `3900`). The server starts
  immediately; changing the port restarts it, unticking stops it. The choice
  persists to `settings.ron`, so it auto-starts next launch.
- Agent writes are applied on the UI thread, so you **watch the structs being
  built live** and can keep editing alongside it.

> ⚠️ The MCP tools **read and write arbitrary target memory** and can attach to
> processes. Keep the server on loopback, only enable it while an agent is
> driving reclass-rs, and treat any connected client as fully trusted.

## Endpoint

```
http://127.0.0.1:<port>/     # default port 3900; POST JSON-RPC
```

Sanity-check it with curl (works with the server enabled, no attach needed):

```sh
curl -s http://127.0.0.1:3900/ \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | jq .result.tools[].name
```

## Connecting a client

Point any streamable-HTTP MCP client at the URL. For Claude Code:

```sh
claude mcp add --transport http reclass-rs http://127.0.0.1:3900/
```

Or as JSON in `.mcp.json` / `~/.claude.json`, alongside your IDA Pro MCP entry:

```json
{
  "mcpServers": {
    "reclass-rs": { "type": "http", "url": "http://127.0.0.1:3900/" },
    "ida-pro-mcp": { "type": "http", "url": "http://127.0.0.1:8744/sse" }
  }
}
```

With both connected, the agent can read a structure in IDA and reproduce it
here: `create_class` → `add_node` per field → `set_address_expr`, then `get_rows`
to read the live values back.

## Tools

| Area | Tools |
|---|---|
| Classes | `list_classes`, `get_class`, `create_class`, `remove_class`, `rename_class`, `set_address_expr` |
| Fields | `add_node`, `insert_node`, `remove_node`, `set_node_kind`, `set_node_name`, `set_node_comment`, `set_array_count`, `add_bytes`, `copy_nodes`, `paste_nodes` |
| Memory | `read_memory`, `write_memory`, `list_regions`, `get_rows` |
| Target | `list_processes`, `attach_pid`, `set_pointer_width` |
| Project | `codegen`, `save_project`, `load_project`, `import_rcnet`, `export_rcnet`, `undo`, `redo` |

Read-only resources mirror the read tools: `reclass://classes`,
`reclass://regions`, `reclass://rows`, `reclass://codegen/rust`,
`reclass://codegen/cpp`.

### Field kinds

A field type (`kind`) is a **shorthand string** — `u8`/`u16`/`u32`/`u64`,
`i8`…`i64`, `f32`, `f64`, `bool`, `ptr`, `fnptr`, `hex8`…`hex64`,
`vec2`/`vec3`/`vec4`, `bits8`…`bits64`, `enum8`…`enum64` (empty variant table),
`cstr`/`wcstr` — **or** a full `NodeKind` JSON object for complex types:

```json
{"Array":{"element":{"Hex":"W64"},"count":8}}
{"ClassPtr":{"class_id":3}}
{"ClassInstance":{"class_id":3}}
{"Text":{"encoding":"Utf8","len":32}}
{"Padding":16}
```

Addresses accept a number or a `0x…` string.

## How it is wired

The server runs on its own thread and **never touches `AppState`**. Sharing it
would force `Send` bounds through the whole memory backend; instead every request
that needs live state is forwarded over an mpsc channel to the GUI thread (the
sole owner of `AppState`), which applies it and replies. That is exactly why MCP
writes show up live.

`GET` (the SSE upgrade path) is answered `405`: the server never initiates
messages. Clients poll; the human watches the GUI.

Implementation and the full tool schema live in
[`crates/app/src/mcp.rs`](../crates/app/src/mcp.rs); the end-to-end test that
speaks real JSON-RPC over a real socket is
[`crates/app/tests/mcp_smoke.rs`](../crates/app/tests/mcp_smoke.rs).
