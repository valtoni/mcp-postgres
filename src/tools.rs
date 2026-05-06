use anyhow::Result;
use tokio_postgres::{Client, SimpleQueryMessage};
use serde_json::json;

pub fn list_tools() -> serde_json::Value {
    json!({
        "tools": [
            {
                "name": "execute_query",
                "description": "Execute arbitrary SQL query. Use this to run DML (SELECT, INSERT, UPDATE) or DDL (CREATE, ALTER, DROP).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The raw SQL query to execute" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "describe_database",
                "description": "Get a schema layout of tables, columns, and types in the database.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "analyze_query_plan",
                "description": "EXPLAIN ANALYZE a query to understand performance bottlenecks.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The SQL query to analyze" }
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
                        "analyze": { "type": "boolean", "description": "Whether to also update statistics (VACUUM ANALYZE)" }
                    },
                    "required": ["table_name"]
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
    // Prefix the user query with EXPLAIN ANALYZE
    let explain_query = format!("EXPLAIN (ANALYZE, COSTS, BUFFERS) {}", query);
    handle_execute_query(client, &explain_query).await
}

pub async fn handle_run_vacuum(client: &Client, table_name: &str, analyze: bool) -> Result<serde_json::Value> {
    // Sanitize table_name naively by wrapping in quotes to prevent simple injections, 
    // although DBA tools assume a trusted admin.
    let safe_table = format!("\"{}\"", table_name.replace("\"", "\"\""));
    let analyze_clause = if analyze { "ANALYZE" } else { "" };
    let vacuum_query = format!("VACUUM {} {};", analyze_clause, safe_table);
    
    handle_execute_query(client, &vacuum_query).await
}
