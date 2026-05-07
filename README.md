# Postgres DBA - MCP Server

A highly optimized Rust-based Model Context Protocol (MCP) server that empowers AI agents to perform advanced Database Administration on PostgreSQL — across **dozens of databases** at once, with static (YAML) and dynamic (Kubernetes / Docker) inventory.

## Começo rápido

Em menos de 2 minutos você consegue:

1. **Build da imagem** (uma vez):
   ```bash
   docker-compose build
   ```

2. **Crie um `databases.yaml`** na raiz do projeto onde o agente vai rodar:
   ```yaml
   version: 1
   default: local
   databases:
     - alias: local
       host: host.docker.internal      # ou IP do seu Postgres
       port: 5432
       user: postgres
       database: postgres
       password_ref: "env://PGPASSWORD"
       source: static
       description: "Postgres do dev local"
   ```
   > Senha **nunca** vai no YAML — só `password_ref` apontando para uma URI (ver §Credenciais).

3. **Exporte a env var** referenciada (ex.: `PGPASSWORD`):
   ```bash
   # PowerShell
   $env:PGPASSWORD = "minha-senha"
   ```

4. **Configure seu cliente MCP**. Snippet pronto para Claude Desktop (`%APPDATA%\Claude\claude_desktop_config.json`):
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
   - `-v %cd%:/project -w /project` → o servidor lê `databases.yaml` do projeto atual.
   - `-v %USERPROFILE%\.kube:/root/.kube:ro` → habilita `discover_k8s_databases`.
   - `-v //./pipe/docker_engine://./pipe/docker_engine` (Windows) → habilita descoberta de containers Docker locais.
   - `-e PGPASSWORD` → repassa a env var para dentro do container.

5. **Valide**: peça ao agente para chamar a tool `list_databases`. Você deve ver o alias `local` com `credentials_loaded: false`. Na primeira chamada de `execute_query` (sem parâmetro `database`, ele usa o default), a senha é resolvida e cacheada na sessão.

6. **(Opcional) Descobrir bases automaticamente**: peça ao agente _"rode `discover_k8s_databases` com `dry_run: true`"_ para ver o que seria adicionado. Sem `dry_run`, atualiza o `databases.yaml`.

## Features

Tools de banco (parâmetro `database` opcional — se omitido usa o `default` do YAML):

- **execute_query** — executa SQL arbitrário (DDL/DML).
- **describe_database** — schema, tabelas e colunas.
- **analyze_query_plan** — `EXPLAIN (ANALYZE, COSTS, BUFFERS)`.
- **run_vacuum** — `VACUUM` / `VACUUM ANALYZE`.

Tools de gerenciamento de inventário:

- **list_databases** — lista todos os aliases configurados + se a credencial já está carregada na sessão.
- **discover_k8s_databases** — varre o cluster Kubernetes (CNPG, Zalando, Bitnami helm, Service:5432) e fontes locais (localhost, Docker), atualizando o `databases.yaml`.
- **set_database_credentials** — fornece a senha para um alias **somente em memória de sessão** (nunca persiste).
- **set_default_database** — atualiza qual alias é o `default` no YAML.

## Configuração: `databases.yaml`

Mora no diretório onde o servidor é executado (mesmo lugar onde antes ficava `.mcp_postgres`).

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

  - alias: cnpg-orders-rw          # gerado por discover_k8s_databases
    host: orders-rw.cnpg-system.svc.cluster.local
    port: 5432
    user: app
    database: app
    password_ref: "k8s-secret://cnpg-system/orders-app/password"
    source: k8s-cnpg
    cluster_ref: "cnpg-system/orders"
    discovered_at: "2026-05-06T10:12:33Z"
```

### Tipos de `password_ref`

| URI                                            | O que faz                                                                |
| ---------------------------------------------- | ------------------------------------------------------------------------ |
| `env://NOME_DA_VAR`                            | Lê a variável de ambiente do processo do servidor.                       |
| `k8s-secret://<namespace>/<name>/<key>`        | Lê (e decodifica base64) o Secret no Kubernetes via kubeconfig montado.  |
| `vault://<path>#<key>`                         | **Schema aceito; resolver real ainda não implementado** — cai em erro orientativo pedindo para usar `set_database_credentials`. |

> **Invariante:** o loader rejeita `databases.yaml` que contenha campo `password` literal — sempre `password_ref`.

## Descoberta dinâmica (Kubernetes + local)

A tool `discover_k8s_databases` executa em paralelo os adapters configurados, dedupa por `(host, port, database)` e mescla no registry. **Aliases com `source: static` nunca são sobrescritos.**

Fontes suportadas:

| Source         | Como detecta                                                                  | `password_ref` gerado                              |
| -------------- | ----------------------------------------------------------------------------- | -------------------------------------------------- |
| `k8s-cnpg`     | CRD `clusters.postgresql.cnpg.io`                                              | `k8s-secret://<ns>/<cluster>-app/password`         |
| `k8s-zalando`  | CRD `postgresqls.acid.zalan.do`                                                | `k8s-secret://<ns>/postgres.<cluster>.credentials.postgresql.acid.zalan.do/password` |
| `k8s-bitnami`  | `Service` com label `app.kubernetes.io/name=postgresql` + Secret `<release>-postgresql` | `k8s-secret://<ns>/<release>-postgresql/<key>` |
| `k8s-generic`  | Qualquer `Service` com porta 5432 + Secret correlato (`-postgresql`, `-postgres`, `-credentials`) | `k8s-secret://<ns>/<secret>/<key>` |
| `local-host`   | Probe TCP em `127.0.0.1:5432` e `host.docker.internal:5432`                   | `env://PGPASSWORD`                                  |
| `local-docker` | Containers com imagem `postgres*` ou `bitnami/postgresql*` com porta exposta  | `env://PGPASSWORD_<NOME>` (você precisa exportar)   |

Parâmetros aceitos:

```json
{
  "include_local": true,
  "dry_run": false,
  "sources": ["k8s-cnpg", "k8s-zalando", "k8s-generic", "local-host", "local-docker"]
}
```

`dry_run: true` reporta `added`/`updated`/`skipped` sem tocar o YAML. Erros parciais (um adapter falhar) **não derrubam** os outros — voltam em `errors[]`.

## Credenciais em memória

Senhas **nunca** são persistidas. Vivem em `RwLock<HashMap<alias, password>>` durante o processo:

- Resolvidas **lazy** na primeira tool que precisa daquele alias.
- Cacheadas até o servidor ser reiniciado.
- Se o `password_ref` falhar (env var ausente, Secret não existe, vault não suportado), o agente pode chamar:

```json
{
  "name": "set_database_credentials",
  "arguments": { "database": "prod-billing", "password": "..." }
}
```

Isso popula a senha na sessão e invalida qualquer conexão antiga para o alias.

## Integração com IDEs

### Claude Desktop

`%APPDATA%\Claude\claude_desktop_config.json` (Windows) ou `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS):

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

Linux/macOS:

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

(✨ **Windows Global Trick**: `cmd.exe /c` força o Node.js client a abrir um shell que expande `%cd%` para o projeto atual — uma única configuração serve todos os repositórios.)

### Cursor IDE / Cline

Settings → Features → MCP → **+ Add New MCP Server**. Use o mesmo comando do snippet acima como **Command**.

### Gemini / Native Agents

Mesma transport `stdio` — paste o JSON de configuração ou use programaticamente:

```javascript
const transport = new StdioClientTransport({
  command: "cmd.exe",
  args: ["/c", "docker run -i --rm -e PGPASSWORD -v %cd%:/project -w /project -v %USERPROFILE%\\.kube:/root/.kube:ro -v //./pipe/docker_engine://./pipe/docker_engine valtoni/mcp-dba-postgres:1.1"]
});
```

## Migrando de `.mcp_postgres`

A versão antiga lia credenciais de um arquivo `.mcp_postgres` no diretório atual. Esse comportamento foi mantido como **fallback**:

- Se existir `databases.yaml` → usado.
- Senão, se existir `.mcp_postgres` → carregado em **memória** como alias sintético `default` (nada é escrito em disco automaticamente).
- Senão → registry vazio. Apenas `discover_k8s_databases` e `set_database_credentials` funcionam até você popular o inventário.

Para migrar formalmente:

1. Copie o template de `databases.yaml` da seção [Começo rápido](#começo-rápido).
2. Mantenha a env var (ex.: `PGPASSWORD`) e use `password_ref: "env://PGPASSWORD"`.
3. Apague `.mcp_postgres` quando confortável (não é necessário — `databases.yaml` tem precedência).

## Troubleshooting

### Connection Refused (os error 111)

Se seu Postgres roda no host (Windows/macOS) e você usava `localhost`, dentro do container `localhost` é o próprio container. Use:

```yaml
host: host.docker.internal
```

### `discover_k8s_databases` retorna `errors: [{source: "k8s-cnpg", ...}]`

Kubeconfig não foi montado (ou aponta para um cluster inacessível). Verifique:

- `-v %USERPROFILE%\.kube:/root/.kube:ro` (Windows) ou `-v $HOME/.kube:/root/.kube:ro` (Linux/macOS) presente nos args do `docker run`.
- `kubectl get ns` funciona no host?

Os outros discoverers continuam funcionando — erros parciais não derrubam o conjunto.

### `local-docker` não encontra nada (Windows)

- O Docker Desktop precisa estar rodando.
- O named pipe precisa estar montado: `-v //./pipe/docker_engine://./pipe/docker_engine`.
- Se ainda assim falhar, o erro aparece em `errors[]` da tool — o resto continua funcionando.

### `password_ref: "env://X"` não resolve

A variável `X` precisa estar **no ambiente do processo Docker**. Passe via:

```bash
docker run ... -e X=meu-valor ... # ou -e X (repassando do host)
```

Ou, durante uma sessão, chame `set_database_credentials` para fornecer a senha em memória.

### Vault

A URI `vault://path#key` é aceita pelo schema mas o resolver ainda não foi implementado. Use `set_database_credentials` por enquanto, ou substitua por `env://VAR` exportada via `--env-file`.
