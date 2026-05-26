# 🐘 Postgres DBA - MCP Server

A highly optimized Rust-based Model Context Protocol (MCP) server that empowers AI agents to perform advanced Database Administration on PostgreSQL. Easily manage **dozens of databases** across local hosts, Docker containers, and live Kubernetes clusters dynamically.

---

## ⚡ Quick Start (3 Steps)

### 1. Configure your keys in `.env`
Create or configure a `.env` file at the root of your project. The MCP server reads this file automatically on startup, eliminating the need to expose plaintext credentials in the YAML registry or host environment variables:
```ini
# Your local database or Vault secrets:
VAULT_ADDR=http://vault.vault.svc.cluster.local:8200
VAULT_TOKEN=hvs.your_active_token
PGPASSWORD=your_local_password
```

### 2. Run the Automatic Installer
Run the installer in your terminal. It verifies your local Docker engine, pulls the official image, and updates your favorite MCP client configuration files with automatic backups.

*   **Windows (PowerShell)**:
    ```powershell
    # Tip: if you use a custom kubeconfig path, define it in your terminal first:
    # $env:KUBECONFIG="C:/work/voxalis/artemis/iac-base/generated/kubeconfig"
    .\setup.ps1
    ```
*   **macOS / Linux / Manual**:
    See the direct `docker run` command in the [Technical Details](TechnicalDetails.md#1-manual-claude-desktop-configuration) guide.

### 3. Talk to your favorite AI Agent!
**Restart your MCP client** (such as Claude Desktop, Claude CLI, Codex CLI, Cursor, or Cline) and ask your agent:
> *"List my databases using the list_databases tool and check if all connections are healthy."*

---

## 📊 Core Capabilities

The Postgres DBA MCP Server exposes a robust suite of administrative tools for database auditing, query optimization, and schema inspection:

*   **`list_databases`**: Lists all configured database aliases and whether their credentials are currently loaded into the active session memory.
*   **`discover_k8s_databases`**: Dynamically scans Kubernetes clusters (CNPG, Zalando, Bitnami, generic services) and local ports to discover active databases and safely merge them into your registry.
*   **`execute_query`**: Executes any arbitrary SQL query (SELECT, DDL, DML) instantly.
*   **`describe_database`**: Returns schemas, tables, columns, and types in seconds.
*   **`analyze_query_plan`**: Runs a full `EXPLAIN (ANALYZE, COSTS, BUFFERS)` to diagnose slow-performing queries.
*   **`show_table_sizes`**: Returns the physical storage sizes on disk of all tables and indexes.
*   **`show_index_stats`**: Reveals detailed index usage, scans, and fetch statistics.
*   **`list_active_queries`**: Lists active database connections and executing queries in real time.

---

## 🔧 Advanced Configuration & Architecture

For manual client setups, detailed `databases.yaml` configuration schemas, supported `password_ref` types, strict Vault resolvers (KV v2, in-memory port-forwarding fallbacks, and short-host DNS enrichment), or comprehensive troubleshooting steps, please consult:

### 📖 [TechnicalDetails.md](TechnicalDetails.md)
