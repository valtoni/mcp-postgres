use anyhow::Result;
use serde_json::json;
use tokio_postgres::{Client, SimpleQueryMessage};

pub fn list_tools() -> serde_json::Value {
    let database_param = json!({
        "type": "string",
        "description": "Alias do databases.yaml. Se omitido, usa o 'default' configurado."
    });

    json!({
        "tools": [
            {
                "name": "execute_query",
                "description": "Execute arbitrary SQL query. Use this to run DML (SELECT, INSERT, UPDATE) or DDL (CREATE, ALTER, DROP).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The raw SQL query to execute" },
                        "database": database_param
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "describe_database",
                "description": "Get a schema layout of tables, columns, and types in the database.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "database": database_param
                    },
                    "required": []
                }
            },
            {
                "name": "analyze_query_plan",
                "description": "EXPLAIN ANALYZE a query to understand performance bottlenecks.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The SQL query to analyze" },
                        "database": database_param
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "run_vacuum",
                "description": "Run VACUUM on a specific table to reclaim storage and update statistics.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "table_name": { "type": "string", "description": "The name of the table to vacuum" },
                        "analyze": { "type": "boolean", "description": "Whether to also update statistics (VACUUM ANALYZE)" },
                        "database": database_param
                    },
                    "required": ["table_name"]
                }
            },
            {
                "name": "list_databases",
                "description": "List all configured databases (aliases) from databases.yaml plus runtime credential status.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "discover_k8s_databases",
                "description": "Discover Postgres databases from the active Kubernetes cluster (CNPG, Zalando, Bitnami helm, Service:5432) and optionally local sources (localhost, Docker). Updates databases.yaml.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "include_local": { "type": "boolean", "description": "Also scan localhost:5432 and local Docker containers (default: true)" },
                        "dry_run": { "type": "boolean", "description": "If true, do not write databases.yaml; just report what would change (default: false)" },
                        "sources": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["k8s-cnpg", "k8s-zalando", "k8s-generic", "local-host", "local-docker"] },
                            "description": "Restrict discovery to these sources. Omit to run all."
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "set_database_credentials",
                "description": "Provide a password for a database alias for the current session only. Never persisted to disk.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "database": { "type": "string", "description": "Alias do databases.yaml" },
                        "password": { "type": "string", "description": "Senha do usuario configurado para o alias" }
                    },
                    "required": ["database", "password"]
                }
            },
            {
                "name": "set_default_database",
                "description": "Update which alias is the 'default' in databases.yaml.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "database": { "type": "string", "description": "Alias do databases.yaml" }
                    },
                    "required": ["database"]
                }
            }
        ]
    })
}

pub async fn handle_execute_query(client: &Client, query: &str) -> Result<serde_json::Value> {
    let messages = client.simple_query(query).await?;

    let mut results = Vec::new();
    let mut current_rows = Vec::new();

    for msg in messages {
        match msg {
            SimpleQueryMessage::Row(row) => {
                let mut row_map = serde_json::Map::new();
                for i in 0..row.len() {
                    let col_name = row.columns()[i].name();
                    let val = row.get(i).map(|s| s.to_string()).unwrap_or_else(|| "null".to_string());
                    row_map.insert(col_name.to_string(), json!(val));
                }
                current_rows.push(json!(row_map));
            }
            SimpleQueryMessage::CommandComplete(info) => {
                let tag = info;
                results.push(json!({
                    "status": "success",
                    "command_tag": tag,
                    "rows": current_rows.clone()
                }));
                current_rows.clear();
            }
            _ => {}
        }
    }

    Ok(json!({
        "content": [
            {
                "type": "text",
                "text": serde_json::to_string_pretty(&results)?
            }
        ]
    }))
}

pub async fn handle_describe_database(client: &Client) -> Result<serde_json::Value> {
    let query = "
        SELECT
            t.table_schema,
            t.table_name,
            c.column_name,
            c.data_type
        FROM information_schema.tables t
        JOIN information_schema.columns c
            ON t.table_name = c.table_name
            AND t.table_schema = c.table_schema
        WHERE t.table_schema NOT IN ('pg_catalog', 'information_schema')
        ORDER BY t.table_schema, t.table_name, c.ordinal_position;
    ";

    handle_execute_query(client, query).await
}

pub async fn handle_analyze_query_plan(client: &Client, query: &str) -> Result<serde_json::Value> {
    let explain_query = format!("EXPLAIN (ANALYZE, COSTS, BUFFERS) {}", query);
    handle_execute_query(client, &explain_query).await
}

pub async fn handle_run_vacuum(client: &Client, table_name: &str, analyze: bool) -> Result<serde_json::Value> {
    let safe_table = format!("\"{}\"", table_name.replace("\"", "\"\""));
    let analyze_clause = if analyze { "ANALYZE" } else { "" };
    let vacuum_query = format!("VACUUM {} {};", analyze_clause, safe_table);

    handle_execute_query(client, &vacuum_query).await
}

pub fn json_response(value: &serde_json::Value) -> serde_json::Value {
    json!({
        "content": [
            { "type": "text", "text": serde_json::to_string_pretty(value).unwrap_or_default() }
        ]
    })
}
