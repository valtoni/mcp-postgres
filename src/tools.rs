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
                "description": "List all configured databases from databases.yaml plus runtime credential status. Note: databases.yaml strictly forbids plain passwords; it uses secure 'password_ref' URIs: 'env://VAR' (loads from the loaded .env file or environment), 'k8s-secret://namespace/secret/key' (dynamically resolved from cluster secrets), or 'vault://path#key' (strictly resolved from the in-cluster HashiCorp Vault using VAULT_ADDR and VAULT_TOKEN environment variables automatically loaded from the project's root .env file). If VAULT_ADDR points to a service inside the cluster (ending in .svc.cluster.local), the MCP server automatically establishes an in-memory port-forward tunnel to it.",
                "inputSchema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "discover_k8s_databases",
                "description": "Discover Postgres databases from the active Kubernetes cluster (CNPG, Zalando, Bitnami helm, Service:5432) and optionally local sources (localhost, Docker). Automatically generates secure 'k8s-secret://...' password references and updates databases.yaml. To override credentials to use HashiCorp Vault, manually update its password_ref in databases.yaml to 'vault://secret/path#key' (which will be resolved strictly via Vault, loading VAULT_ADDR and VAULT_TOKEN from the project's root .env file).",
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
                "description": "Provide a password in-memory for a database alias for the current session only. Never persisted. Use this if the configured 'password_ref' (env://, k8s-secret://, vault://) is missing or auth fails.",
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
            },
            {
                "name": "show_table_sizes",
                "description": "Get the physical size on disk of all tables and their indexes in the database.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "database": database_param
                    },
                    "required": []
                }
            },
            {
                "name": "show_index_stats",
                "description": "Get statistics about index scans and usage to identify unused or low-performance indexes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "database": database_param
                    },
                    "required": []
                }
            },
            {
                "name": "list_active_queries",
                "description": "List currently active database connections, their executing queries, and whether they are blocked.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "database": database_param
                    },
                    "required": []
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

pub async fn handle_show_table_sizes(client: &Client) -> Result<serde_json::Value> {
    let query = "
        SELECT 
            relname AS table_name,
            pg_size_pretty(pg_total_relation_size(c.oid)) AS total_size,
            pg_size_pretty(pg_relation_size(c.oid)) AS table_size,
            pg_size_pretty(pg_total_relation_size(c.oid) - pg_relation_size(c.oid)) AS index_size
        FROM pg_class c
        LEFT JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
          AND c.relkind = 'r'
        ORDER BY pg_total_relation_size(c.oid) DESC;
    ";
    handle_execute_query(client, query).await
}

pub async fn handle_show_index_stats(client: &Client) -> Result<serde_json::Value> {
    let query = "
        SELECT
            schemaname,
            relname AS table_name,
            indexrelname AS index_name,
            idx_scan AS number_of_scans,
            idx_tup_read AS tuples_read,
            idx_tup_fetch AS tuples_fetched
        FROM pg_stat_user_indexes
        ORDER BY idx_scan ASC;
    ";
    handle_execute_query(client, query).await
}

pub async fn handle_list_active_queries(client: &Client) -> Result<serde_json::Value> {
    let query = "
        SELECT
            pid,
            usename AS user_name,
            client_addr AS client_ip,
            backend_start,
            query_start,
            state,
            wait_event_type,
            wait_event,
            query
        FROM pg_stat_activity
        WHERE state != 'idle' AND pid != pg_backend_pid();
    ";
    handle_execute_query(client, query).await
}

pub fn json_response(value: &serde_json::Value) -> serde_json::Value {
    json!({
        "content": [
            { "type": "text", "text": serde_json::to_string_pretty(value).unwrap_or_default() }
        ]
    })
}
