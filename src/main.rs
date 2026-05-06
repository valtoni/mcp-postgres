mod db;
mod mcp_protocol;
mod tools;

use anyhow::Result;
use mcp_protocol::{make_error, make_response, RpcMessage};
use serde_json::json;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("Postgres MCP Server starting on stdio...");

    let client = db::connect().await?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let msg = match serde_json::from_str::<RpcMessage>(&line) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("Failed to parse message: {}. Line: {}", e, line);
                continue;
            }
        };

        match msg {
            RpcMessage::Request(req) => {
                match req.method.as_str() {
                    "initialize" => {
                        let response = make_response(req.id, json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {
                                "tools": {}
                            },
                            "serverInfo": {
                                "name": "mcp-dba-postgres",
                                "version": "1.0.0"
                            }
                        }));
                        send_response(&mut stdout, &response).await?;
                    }
                    "tools/list" => {
                        let result = tools::list_tools();
                        let response = make_response(req.id, result);
                        send_response(&mut stdout, &response).await?;
                    }
                    "tools/call" => {
                        let mut response_val = None;
                        
                        if let Some(params) = req.params {
                            if let Some(name) = params.get("name").and_then(|n| n.as_str()) {
                                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                                
                                match name {
                                    "execute_query" => {
                                        if let Some(query) = arguments.get("query").and_then(|q| q.as_str()) {
                                            match tools::handle_execute_query(&client, query).await {
                                                Ok(res) => response_val = Some(make_response(req.id.clone(), res)),
                                                Err(e) => response_val = Some(make_error(req.id.clone(), -32000, &e.to_string())),
                                            }
                                        } else {
                                            response_val = Some(make_error(req.id.clone(), -32602, "Missing 'query' parameter"));
                                        }
                                    }
                                    "describe_database" => {
                                        match tools::handle_describe_database(&client).await {
                                            Ok(res) => response_val = Some(make_response(req.id.clone(), res)),
                                            Err(e) => response_val = Some(make_error(req.id.clone(), -32000, &e.to_string())),
                                        }
                                    }
                                    "analyze_query_plan" => {
                                        if let Some(query) = arguments.get("query").and_then(|q| q.as_str()) {
                                            match tools::handle_analyze_query_plan(&client, query).await {
                                                Ok(res) => response_val = Some(make_response(req.id.clone(), res)),
                                                Err(e) => response_val = Some(make_error(req.id.clone(), -32000, &e.to_string())),
                                            }
                                        } else {
                                            response_val = Some(make_error(req.id.clone(), -32602, "Missing 'query' parameter"));
                                        }
                                    }
                                    "run_vacuum" => {
                                        if let Some(table_name) = arguments.get("table_name").and_then(|t| t.as_str()) {
                                            let analyze = arguments.get("analyze").and_then(|a| a.as_bool()).unwrap_or(false);
                                            match tools::handle_run_vacuum(&client, table_name, analyze).await {
                                                Ok(res) => response_val = Some(make_response(req.id.clone(), res)),
                                                Err(e) => response_val = Some(make_error(req.id.clone(), -32000, &e.to_string())),
                                            }
                                        } else {
                                            response_val = Some(make_error(req.id.clone(), -32602, "Missing 'table_name' parameter"));
                                        }
                                    }
                                    _ => {
                                        response_val = Some(make_error(req.id.clone(), -32601, "Tool not found"));
                                    }
                                }
                            }
                        }

                        if let Some(resp) = response_val {
                            send_response(&mut stdout, &resp).await?;
                        } else {
                            let resp = make_error(req.id, -32600, "Invalid tools/call request parameters");
                            send_response(&mut stdout, &resp).await?;
                        }
                    }
                    _ => {
                        let resp = make_error(req.id, -32601, "Method not found");
                        send_response(&mut stdout, &resp).await?;
                    }
                }
            }
            RpcMessage::Notification(notif) => {
                if notif.method == "initialized" {
                    eprintln!("Client initialized");
                }
            }
            RpcMessage::Response(_) => {
                // Not expected from client via stdin loop usually
            }
        }
    }

    Ok(())
}

async fn send_response<T: serde::Serialize>(stdout: &mut io::Stdout, response: &T) -> Result<()> {
    let mut out = serde_json::to_string(response)?;
    out.push('\n');
    stdout.write_all(out.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}
