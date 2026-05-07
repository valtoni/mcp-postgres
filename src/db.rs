// Modulo legado mantido apenas como documentacao. A logica real vive em:
// - registry.rs : modelo do databases.yaml + fallback .mcp_postgres
// - credentials.rs : resolucao de password_ref em memoria
// - pool.rs : cache lazy de conexoes Postgres por alias (incluindo connect_with)
