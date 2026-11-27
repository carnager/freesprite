mod color;
mod draw;
mod export;
mod lint;
mod model;
mod render;
mod tools;
#[cfg(test)]
mod tests;

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None | Some("mcp") => serve(),
        Some("--version" | "-V") => {
            println!("freesprite {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown command '{other}'\nusage: freesprite [mcp]   (runs the MCP server on stdio)");
            std::process::exit(2)
        }
    }
}

/// MCP over stdio: newline-delimited JSON-RPC 2.0.
fn serve() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut ws = tools::Workspace::default();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(&mut out, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}))?;
                continue;
            }
        };
        let msgs = match msg {
            Value::Array(a) => a,
            m => vec![m],
        };
        for m in msgs {
            if let Some(resp) = handle(&mut ws, m) {
                send(&mut out, &resp)?;
            }
        }
    }
    Ok(())
}

fn send(out: &mut impl Write, v: &Value) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *out, v)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn handle(ws: &mut tools::Workspace, m: Value) -> Option<Value> {
    let method = m.get("method")?.as_str()?.to_string();
    let id = m.get("id")?.clone(); // notifications have no id -> no reply
    let params = m.get("params").cloned().unwrap_or(json!({}));
    let result: Result<Value, (i64, String)> = match method.as_str() {
        "initialize" => {
            let pv = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            Ok(json!({
                "protocolVersion": pv,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "freesprite", "version": env!("CARGO_PKG_VERSION")},
                "instructions": tools::INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools::definitions()})),
        "tools/call" => match params.get("name").and_then(Value::as_str) {
            Some(name) => Ok(ws.call(name, params.get("arguments").cloned().unwrap_or(json!({})))),
            None => Err((-32602, "missing tool name".into())),
        },
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    })
}
