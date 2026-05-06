# Postgres DBA - MCP Server

A highly optimized Rust-based Model Context Protocol (MCP) server that empowers AI agents to perform advanced Database Administration on PostgreSQL.

## Features

- **execute_query**: Run arbitrary DDL/DML.
- **describe_database**: Retrieve schema, tables, and column type breakdowns.
- **analyze_query_plan**: Automatically runs `EXPLAIN (ANALYZE, COSTS, BUFFERS)` to diagnose bottlenecks.
- **run_vacuum**: Execute `VACUUM` and `VACUUM ANALYZE` on specific tables to prune bloat.

## Deployment & Running
To keep the binary and execution footprint as tiny as possible, this project is distributed as a multi-stage Docker image based on Debian Slim (with the `postgresql-client` baked in for dump/restore functionality).

### Quickstart

1. To use this MCP server autonomously across different projects, create a `.mcp_postgres` file directly in the root of your target project:
```bash
# Inside your random project folder that needs DBA capabilities
PGUSER=root
PGPASSWORD=secret
PGHOST=db.internal
PGDATABASE=app_db
PGPORT=5432
```
The MCP Server will automatically detect and load credentials from this `.mcp_postgres` if it exists in the folder it is executed from!

2. Build the Docker image locally once:
```bash
docker-compose build
```

### 3. How-To: IDE & Agent Integration

Since the MCP uses `stdio` to communicate, you must configure your AI client to execute the Docker container. To ensure the container can see your project's `.mcp_postgres` credentials, map the current working directory as a volume (`-v`) and set it as the working dir (`-w`).

#### Claude Desktop
Add the following to your `claude_desktop_config.json` (located at `%APPDATA%\Claude` on Windows or `~/Library/Application Support/Claude` on macOS):

```json
{
  "mcpServers": {
    "mcp-dba-postgres": {
      "command": "cmd.exe",
      "args": [
        "/c",
        "docker run -i --rm -v %cd%:/project -w /project valtoni/mcp-dba-postgres:1.0"
      ]
    }
  }
}
```
*(✨ **Windows Global Trick**: By using `cmd.exe /c` as the command instead of directly calling `docker`, we force the Node.js client to spin up a shell. This natively evaluates `%cd%` to whatever project folder you are currently running Claude Code from, allowing you to use this single `.claude.json` globally across all your projects without hardcoding paths!)*

#### Cursor IDE / Cline (Codex)
1. Navigate to **Settings > Features > MCP** in Cursor (or use the Cline configuration).
2. Click **+ Add New MCP Server**.
3. Fill in the details:
   - **Name:** `mcp-dba-postgres`
   - **Type:** `command`
   - **Command:** `cmd.exe /c docker run -i --rm -v %cd%:/project -w /project valtoni/mcp-dba-postgres:1.0`

#### Gemini / Native Agents
For Gemini (via tools like Antigravity, Google AI Studio, or custom scripts), the integration follows the standard `stdio` transport. If you are using a UI that supports configuration files, paste the JSON above.

For Node.js `@modelcontextprotocol/sdk` programmatic usage with Gemini:
```javascript
const transport = new StdioClientTransport({
  command: "cmd.exe",
  args: ["/c", "docker run -i --rm -v %cd%:/project -w /project valtoni/mcp-dba-postgres:1.0"]
});
```

### 4. Troubleshooting: Connection Refused (os error 111)

If your database is running directly on your Windows/Mac host machine (i.e., you normally connect to `localhost:5432`), the MCP Server will fail to connect and emit an `os error 111`. 

**Why?** Because the MCP Server runs *inside* an isolated Docker container, so `localhost` means the container itself, not your host computer!

**Fix:** In your project's `.mcp_postgres` file, change `PGHOST=localhost` to the special Docker DNS:
```bash
PGHOST=host.docker.internal
```
