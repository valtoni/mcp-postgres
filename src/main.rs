mod credentials;
mod ctl;
mod db;
mod discovery;
mod mcp_protocol;
mod pool;
mod registry;
mod tools;
mod tools_registry;

use anyhow::Result;
use mcp_protocol::{make_error, make_response, RpcMessage};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::credentials::CredentialStore;
use crate::discovery::k8s_client::K8sHandle;
use crate::pool::ConnectionPool;
use crate::registry::Registry;
use crate::tools_registry::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "ctl" {
        return ctl::run_ctl(&args[2..]).await;
    }

    eprintln!("Postgres MCP Server starting on stdio...");

    let cwd = std::env::current_dir()?;
    let registry = Arc::new(Registry::load_or_legacy(&cwd).await?);
    let k8s = K8sHandle::new();
    let credentials = Arc::new(CredentialStore::new(k8s.clone()));
    let pool = Arc::new(ConnectionPool::new(registry.clone(), credentials.clone()));

    let state = Arc::new(AppState {
        registry,
        credentials,
        pool,
        k8s,
        cwd,
    });

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
            RpcMessage::Request(req) => match req.method.as_str() {
                "initialize" => {
                    let response = make_response(
                        req.id,
                        json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": { "tools": {} },
                            "serverInfo": {
                                "name": "mcp-dba-postgres",
                                "version": "1.1.0"
                            }
                        }),
                    );
                    send_response(&mut stdout, &response).await?;
                }
                "tools/list" => {
                    let response = make_response(req.id, tools::list_tools());
                    send_response(&mut stdout, &response).await?;
                }
                "tools/call" => {
                    let id = req.id.clone();
                    let response_val = handle_tools_call(&state, req.params).await;
                    let response = match response_val {
                        Ok(value) => make_response(id, value),
                        Err((code, msg)) => make_error(id, code, &msg),
                    };
                    send_response(&mut stdout, &response).await?;
                }
                _ => {
                    let resp = make_error(req.id, -32601, "Method not found");
                    send_response(&mut stdout, &resp).await?;
                }
            },
            RpcMessage::Notification(notif) => {
                if notif.method == "initialized" {
                    eprintln!("Client initialized");
                }
            }
            RpcMessage::Response(_) => {}
        }
    }

    Ok(())
}

async fn handle_tools_call(
    state: &Arc<AppState>,
    params: Option<Value>,
) -> std::result::Result<Value, (i64, String)> {
    let params = params.ok_or((-32600, "Invalid tools/call request parameters".to_string()))?;
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or((-32600, "Missing 'name'".to_string()))?
        .to_string();
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    match name.as_str() {
        "list_databases" => tools_registry::handle_list_databases(state)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        "discover_k8s_databases" => tools_registry::handle_discover_k8s_databases(state, &arguments)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        "set_database_credentials" => tools_registry::handle_set_database_credentials(state, &arguments)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        "set_default_database" => tools_registry::handle_set_default_database(state, &arguments)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        "execute_query" | "describe_database" | "analyze_query_plan" | "run_vacuum" | "show_table_sizes" | "show_index_stats" | "list_active_queries" => {
            run_query_tool(state, &name, &arguments).await
        }
        _ => Err((-32601, format!("Tool '{}' nao encontrada", name))),
    }
}

async fn run_query_tool(
    state: &Arc<AppState>,
    name: &str,
    arguments: &Value,
) -> std::result::Result<Value, (i64, String)> {
    let alias = match arguments.get("database").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => state
            .registry
            .default_alias()
            .await
            .ok_or((
                -32001,
                "Nenhum 'database' informado e nenhum default configurado. \
                 Crie um databases.yaml ou rode 'discover_k8s_databases'."
                    .to_string(),
            ))?,
    };

    let client = state
        .pool
        .get(&alias)
        .await
        .map_err(|e| (-32001, format!("falha resolvendo alias '{}': {:#}", alias, e)))?;

    match name {
        "execute_query" => {
            let query = arguments
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or((-32602, "Missing 'query' parameter".to_string()))?;
            tools::handle_execute_query(&client, query)
                .await
                .map_err(|e| (-32000, format!("{:#}", e)))
        }
        "describe_database" => tools::handle_describe_database(&client)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        "analyze_query_plan" => {
            let query = arguments
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or((-32602, "Missing 'query' parameter".to_string()))?;
            tools::handle_analyze_query_plan(&client, query)
                .await
                .map_err(|e| (-32000, format!("{:#}", e)))
        }
        "run_vacuum" => {
            let table_name = arguments
                .get("table_name")
                .and_then(|t| t.as_str())
                .ok_or((-32602, "Missing 'table_name' parameter".to_string()))?;
            let analyze = arguments
                .get("analyze")
                .and_then(|a| a.as_bool())
                .unwrap_or(false);
            tools::handle_run_vacuum(&client, table_name, analyze)
                .await
                .map_err(|e| (-32000, format!("{:#}", e)))
        }
        "show_table_sizes" => tools::handle_show_table_sizes(&client)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        "show_index_stats" => tools::handle_show_index_stats(&client)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        "list_active_queries" => tools::handle_list_active_queries(&client)
            .await
            .map_err(|e| (-32000, format!("{:#}", e))),
        _ => Err((-32601, format!("Tool '{}' nao encontrada", name))),
    }
}

async fn send_response<T: serde::Serialize>(stdout: &mut io::Stdout, response: &T) -> Result<()> {
    let mut out = serde_json::to_string(response)?;
    out.push('\n');
    stdout.write_all(out.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}
