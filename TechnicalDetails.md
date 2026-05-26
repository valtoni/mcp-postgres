# 🛠️ Postgres DBA - Technical Details

This document contains the deep technical specifications, manual configuration guides, architectural layout, and troubleshooting steps for the `mcp-dba-postgres` server.

---

## 🏗️ Architecture & Core Mechanics

The Postgres DBA MCP server is built in Rust using Tokio for an asynchronous, high-concurrency runtime. It connects host-level AI clients (running in a local shell or IDE sandbox) to both local and in-cluster Kubernetes database topographies.

```
                  ┌───────────────────────────────────────────────────┐
                  │                 IDE / AI Client                   │
                  │        (Claude, Codex, Gemini, Cline, etc.)       │
                  └─────────────────────────┬─────────────────────────┘
                                            │ stdio JSON-RPC
                                            ▼
                  ┌───────────────────────────────────────────────────┐
                  │          mcp-dba-postgres MCP Server              │
                  │             (Local Docker Engine)                 │
                  └──────┬─────────────────────────────────────┬──────┘
                         │                                     │
     Local network       │                                     │ Kubernetes API
     (TCP & named pipes) │                                     │ (via Kubeconfig mount)
                         ▼                                     ▼
         ┌───────────────────────────────┐     ┌───────────────────────────────┐
         │ - Local Postgres (5432)       │     │ - In-Cluster Vault Service    │
         │ - Docker Engine containers    │     │ - Dynamic K8s Service Pods   │
         │   (via docker_engine pipe)    │     │   (via Port-Forward streams)  │
         └───────────────────────────────┘     └───────────────────────────────┘
```

### 1. In-Memory Session Credential Store
To ensure absolute security, **passwords are never saved to disk**. They live inside a thread-safe `RwLock<HashMap<String, Credentials>>` in-memory.
* **Lazy Resolution**: Secrets are fetched only on the first active query call targeting that database alias.
* **Session Lifetime**: Secrets are cached for the lifetime of the running MCP server process. Restarting the client or calling `set_database_credentials` resets the cache.

---

## 🔒 Secure `password_ref` Resolvers

The server completely forbids writing plaintext passwords in `databases.yaml`. Instead, it parses secure URIs in the `password_ref` field:

### 1. Environment Variable (`env://VAR_NAME`)
* **How it works**: Reads the variable from the process environment.
* **Local .env integration**: On startup, the server automatically reads variables from the project's local `.env` file (mounted at `/project/.env` inside the container) using the `dotenvy` library, populating the environment.

### 2. Kubernetes Secret (`k8s-secret://<namespace>/<name>/<key>`)
* **How it works**: Queries the Kubernetes API using the mounted Kubeconfig context, retrieves the Secret, decodes the base64 value, and yields it.

### 3. HashiCorp Vault (`vault://<path>#<key>`)
* **Strict Mode**: The resolved secret is fetched strictly via the Vault API. There is no environment fallback to ensure absolute safety.
* **KV v2 Generic Support**: Automatically splits the path at the first `/` (e.g. `secret/meridian/br/hmg/db` is split into `secret` and `meridian/br/hmg/db`), inserting `data/` after the first segment (`secret/data/meridian/br/hmg/db`) to support KV v2 double-nesting (`.data.data.<key>`) for any custom mount point.
* **Network-Resilient Connectivity & Port-Forward Fallback**:
  * Strips URL schemes and detects appropriate ports (`443` for standard `https://`, `80` for `http://`, or custom ports).
  * If the host matches `.svc.cluster.local` or the special cluster domain `vault.vox`, it opens a programmatic port-forward stream to the active Vault Pod inside the cluster using the Kubernetes WebSocket API.
  * **Fallback**: If the direct TCP connection to the host fails, the server automatically falls back to attempting an in-cluster K8s port-forward tunnel to `vault.vault.svc.cluster.local:8200` to locate the cluster's Vault service.

---

## 🔄 DSN Override & Automatic Port-Forwarding

When a `password_ref` (such as a Vault URI) resolves to a full PostgreSQL Connection String (DSN) starting with `postgresql://` or `postgres://`, the MCP server applies advanced routing mechanics:

1. **DSN Parsing**: Breaks down the connection string into `host`, `port`, `user`, `password`, and `database`.
2. **Dynamic Override**: Replaces all statically configured parameters in `databases.yaml` with the resolved parameters from the DSN (enabling, for instance, switching connection users dynamically from `app` to `meridian`).
3. **Short-Host Namespace Enrichment**:
   * If the parsed DSN host is a short, inside-the-cluster service name (e.g., `meridian-pg-rw`), but the original host in `databases.yaml` was fully qualified (e.g., `meridian-pg-rw.meridian-br-hmg.svc.cluster.local`), the MCP server automatically appends the original namespace and suffix to the DSN host.
   * This is also triggered if the entry has a `cluster_ref` (e.g. `meridian-br-hmg/meridian-pg`).
4. **WebSocket Port-Forward Stream**:
   Once the fully-qualified `.svc.cluster.local` host is reconstructed, the MCP server connects directly to the Kubernetes API, extracts the active backing Pod from the service's `Endpoints`, opens a raw WebSocket stream to port `5432` on that Pod, and routes the `tokio_postgres` connection directly over this in-memory channel. **No local ports are opened on the host, preventing all conflicts.**

---

## 📂 Configuration: `databases.yaml`

The registry configuration file `databases.yaml` lives in the project folder where the MCP server is initialized.

```yaml
version: 1
default: prod-billing
databases:
  - alias: prod-billing
    host: db.prod.internal
    port: 5432
    user: app_ro
    database: billing
    password_ref: "env://PROD_BILLING_PWD"
    source: static
    description: "Read-only billing replica"

  - alias: cnpg-meridian-br-hmg-meridian-pg
    host: meridian-pg-rw.meridian-br-hmg.svc.cluster.local
    port: 5432
    user: meridian
    database: meridian
    password_ref: "vault://secret/meridian/br/hmg/db#DIRECT_URL"
    source: k8s-cnpg
    cluster_ref: "meridian-br-hmg/meridian-pg"
```

> ⚠️ **Critical Security Invariant**: The YAML parser will throw a validation error and refuse to start if a plaintext `password` field is found in the file. All passwords must use a secure `password_ref`.

---

## 🔄 Dynamic Discovery Adapters

The `discover_k8s_databases` tool executes multiple parallel discoverer adapters to find active databases:

| Adapter | Discovery Mechanism | Generated `password_ref` |
| --- | --- | --- |
| `k8s-cnpg` | Watches custom resource definition `clusters.postgresql.cnpg.io` | `k8s-secret://<namespace>/<cluster>-app/password` |
| `k8s-zalando` | Watches custom resource definition `postgresqls.acid.zalan.do` | `k8s-secret://<namespace>/postgres.<cluster>.credentials.postgresql.acid.zalan.do/password` |
| `k8s-bitnami` | Scans `Services` matching label `app.kubernetes.io/name=postgresql` | `k8s-secret://<namespace>/<service-name>/password` |
| `k8s-generic` | Scans any `Service` on port `5432` and correlates secret naming | `k8s-secret://<namespace>/<secret-name>/password` |
| `local-host` | Performs high-speed TCP pings on `127.0.0.1` and `host.docker.internal` | `env://PGPASSWORD` |
| `local-docker` | Queries Docker API socket for containers running Postgres images | `env://PGPASSWORD_<CONTAINER_NAME>` |

---

## 🛠️ Manual CLI Installation & Setup

If you prefer to configure clients manually without using `setup.ps1` or the `ctl` subcommands:

### 1. Docker Build (Optional)
```bash
# Compile and build the production-ready Rust container locally
docker build -t valtoni/mcp-dba-postgres:1.1 .
```

### 2. Manual Claude Desktop Configuration
Add the server under `mcpServers` inside `%APPDATA%\Claude\claude_desktop_config.json` (Windows) or `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS/Linux):

#### Windows (using global trick to dynamically resolve current directory `%cd%`):
```json
{
  "mcpServers": {
    "mcp-dba-postgres": {
      "command": "cmd.exe",
      "args": [
        "/c",
        "docker run -i --rm -e PGPASSWORD -v %cd%:/project -w /project -v %USERPROFILE%\\.kube:/root/.kube:ro -v //./pipe/docker_engine://./pipe/docker_engine valtoni/mcp-dba-postgres:1.1"
      ]
    }
  }
}
```

*If using a custom Kubeconfig location:*
```json
"args": [
  "/c",
  "docker run -i --rm -e PGPASSWORD -v %cd%:/project -w /project -e KUBECONFIG=/kubeconfig -v C:/caminho/para/seu/kubeconfig:/kubeconfig:ro -v //./pipe/docker_engine://./pipe/docker_engine valtoni/mcp-dba-postgres:1.1"
]
```

#### macOS / Linux:
```json
{
  "mcpServers": {
    "mcp-dba-postgres": {
      "command": "sh",
      "args": [
        "-c",
        "docker run -i --rm -e PGPASSWORD -v $PWD:/project -w /project -v $HOME/.kube:/root/.kube:ro -v /var/run/docker.sock:/var/run/docker.sock valtoni/mcp-dba-postgres:1.1"
      ]
    }
  }
}
```

---

## 🔧 Troubleshooting

### 1. `failed to lookup address information: Name or service not known`
* **Cause**: A database host (like `.svc.cluster.local`) or a short host (like `meridian-pg-rw`) was resolved from a DSN but failed to activate the port-forwarder.
* **Solution**: Ensure your `databases.yaml` entry has the fully-qualified in-cluster host (ending in `.svc.cluster.local`) or a valid `cluster_ref` (e.g. `namespace/name`). The MCP server will automatically enrich the DSN host name and trigger the K8s WebSocket tunnel.

### 2. `Connection Refused (os error 111) on host.docker.internal`
* **Cause**: The container was unable to connect to the host's localhost loopback.
* **Solution**: On Windows and macOS, ensure Docker Desktop has "Expose daemon on tcp://localhost:2375 without TLS" enabled, or that your database host is set to `host.docker.internal` instead of `localhost` or `127.0.0.1`.

### 3. `VAULT_TOKEN not found in environment`
* **Cause**: The Vault token was not resolved inside the container.
* **Solution**: Declare `VAULT_TOKEN` and `VAULT_ADDR` inside a `.env` file at the root of your workspace. Because the workspace is mounted as `/project`, the MCP server's integrated `dotenvy` automatically loads the `.env` file into the container process.
