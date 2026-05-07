use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::sync::RwLock;

pub const REGISTRY_FILENAME: &str = "databases.yaml";
pub const LEGACY_DOTFILE: &str = ".mcp_postgres";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Static,
    K8sCnpg,
    K8sZalando,
    K8sGeneric,
    K8sBitnami,
    LocalHost,
    LocalDocker,
    Legacy,
    Manual,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Static => "static",
            Source::K8sCnpg => "k8s-cnpg",
            Source::K8sZalando => "k8s-zalando",
            Source::K8sGeneric => "k8s-generic",
            Source::K8sBitnami => "k8s-bitnami",
            Source::LocalHost => "local-host",
            Source::LocalDocker => "local-docker",
            Source::Legacy => "legacy",
            Source::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseEntry {
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub password_ref: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub databases: Vec<DatabaseEntry>,
}

fn default_version() -> u32 {
    1
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            version: 1,
            default: None,
            databases: Vec::new(),
        }
    }
}

pub struct Registry {
    inner: RwLock<RegistryState>,
}

struct RegistryState {
    file: RegistryFile,
    path: Option<PathBuf>,
    persistent: bool,
}

impl Registry {
    pub fn new_in_memory(file: RegistryFile) -> Self {
        Self {
            inner: RwLock::new(RegistryState {
                file,
                path: None,
                persistent: false,
            }),
        }
    }

    pub async fn load_or_legacy(cwd: &Path) -> Result<Self> {
        let yaml_path = cwd.join(REGISTRY_FILENAME);
        if yaml_path.exists() {
            let raw = fs::read_to_string(&yaml_path)
                .await
                .with_context(|| format!("Falha lendo {}", yaml_path.display()))?;
            let file = parse_registry(&raw)?;
            return Ok(Self {
                inner: RwLock::new(RegistryState {
                    file,
                    path: Some(yaml_path),
                    persistent: true,
                }),
            });
        }

        let legacy = cwd.join(LEGACY_DOTFILE);
        if legacy.exists() {
            eprintln!(
                "Nenhum {} encontrado; usando {} como fallback (registro em memoria)",
                REGISTRY_FILENAME, LEGACY_DOTFILE
            );
            let _ = dotenvy::from_path(&legacy);
            let entry = entry_from_env("default", Source::Legacy);
            let mut file = RegistryFile::default();
            file.default = Some(entry.alias.clone());
            file.databases.push(entry);
            return Ok(Self::new_in_memory(file));
        }

        eprintln!(
            "Nenhum {} ou {} encontrado em {}. Registro vazio.",
            REGISTRY_FILENAME,
            LEGACY_DOTFILE,
            cwd.display()
        );
        Ok(Self::new_in_memory(RegistryFile::default()))
    }

    pub async fn list(&self) -> Vec<DatabaseEntry> {
        self.inner.read().await.file.databases.clone()
    }

    pub async fn default_alias(&self) -> Option<String> {
        let state = self.inner.read().await;
        state.file.default.clone().or_else(|| {
            state
                .file
                .databases
                .first()
                .map(|d| d.alias.clone())
        })
    }

    pub async fn get(&self, alias: &str) -> Option<DatabaseEntry> {
        self.inner
            .read()
            .await
            .file
            .databases
            .iter()
            .find(|d| d.alias == alias)
            .cloned()
    }

    pub async fn set_default(&self, alias: &str) -> Result<()> {
        let mut state = self.inner.write().await;
        if !state.file.databases.iter().any(|d| d.alias == alias) {
            bail!("alias '{}' nao existe no registry", alias);
        }
        state.file.default = Some(alias.to_string());
        persist_if_needed(&state).await
    }

    pub async fn merge_discovered(
        &self,
        discovered: Vec<DatabaseEntry>,
        dry_run: bool,
    ) -> Result<MergeReport> {
        let mut state = self.inner.write().await;
        let mut report = MergeReport::default();

        for new_entry in discovered {
            let dedup_key = (
                new_entry.host.clone(),
                new_entry.port,
                new_entry.database.clone(),
            );

            let existing_idx = state.file.databases.iter().position(|d| {
                (d.host.clone(), d.port, d.database.clone()) == dedup_key
            });

            match existing_idx {
                Some(idx) => {
                    let existing = &state.file.databases[idx];
                    if existing.source == Source::Static {
                        report.skipped.push(SkipReason {
                            alias: existing.alias.clone(),
                            reason: "alias estatico preservado".to_string(),
                        });
                        continue;
                    }
                    let alias = existing.alias.clone();
                    if !dry_run {
                        let mut updated = new_entry.clone();
                        updated.alias = alias.clone();
                        state.file.databases[idx] = updated;
                    }
                    report.updated.push(alias);
                }
                None => {
                    let mut entry = new_entry;
                    entry.alias = unique_alias(&state.file.databases, &entry.alias);
                    let alias = entry.alias.clone();
                    if !dry_run {
                        state.file.databases.push(entry);
                    }
                    report.added.push(alias);
                }
            }
        }

        if !dry_run {
            if state.file.default.is_none() {
                if let Some(first) = state.file.databases.first() {
                    state.file.default = Some(first.alias.clone());
                }
            }
            persist_if_needed(&state).await?;
        }

        Ok(report)
    }

    pub async fn ensure_persistent(&self, cwd: &Path) -> Result<()> {
        let mut state = self.inner.write().await;
        if state.path.is_none() {
            state.path = Some(cwd.join(REGISTRY_FILENAME));
        }
        state.persistent = true;
        persist_if_needed(&state).await
    }

    pub async fn is_persistent(&self) -> bool {
        self.inner.read().await.persistent
    }
}

#[derive(Debug, Default, Serialize)]
pub struct MergeReport {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<SkipReason>,
}

#[derive(Debug, Serialize)]
pub struct SkipReason {
    pub alias: String,
    pub reason: String,
}

fn entry_from_env(alias: &str, source: Source) -> DatabaseEntry {
    let host = std::env::var("PGHOST").unwrap_or_else(|_| "localhost".to_string());
    let user = std::env::var("PGUSER").unwrap_or_else(|_| "postgres".to_string());
    let database = std::env::var("PGDATABASE").unwrap_or_else(|_| "postgres".to_string());
    let port = std::env::var("PGPORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5432);

    DatabaseEntry {
        alias: alias.to_string(),
        host,
        port,
        user,
        database,
        password_ref: "env://PGPASSWORD".to_string(),
        source,
        description: Some("Importado do .mcp_postgres legado".to_string()),
        cluster_ref: None,
        container_id: None,
        discovered_at: None,
    }
}

fn parse_registry(raw: &str) -> Result<RegistryFile> {
    let raw_value: serde_yaml::Value = serde_yaml::from_str(raw)
        .context("databases.yaml com YAML invalido")?;

    if let serde_yaml::Value::Mapping(ref map) = raw_value {
        if let Some(serde_yaml::Value::Sequence(ref seq)) =
            map.get(serde_yaml::Value::String("databases".to_string()))
        {
            for entry in seq {
                if let serde_yaml::Value::Mapping(em) = entry {
                    if em.contains_key(serde_yaml::Value::String("password".to_string())) {
                        bail!(
                            "campo 'password' literal proibido em databases.yaml \
                             (use 'password_ref' com env://, k8s-secret:// ou vault://)"
                        );
                    }
                }
            }
        }
    }

    let file: RegistryFile =
        serde_yaml::from_value(raw_value).context("databases.yaml falhou na desserializacao")?;

    if file.version != 1 {
        bail!("databases.yaml: 'version' deve ser 1 (encontrado {})", file.version);
    }

    let mut seen = BTreeMap::new();
    for db in &file.databases {
        if !is_valid_password_ref(&db.password_ref) {
            bail!(
                "alias '{}': password_ref invalida '{}' (esperado env://, k8s-secret:// ou vault://)",
                db.alias,
                db.password_ref
            );
        }
        if let Some(prev) = seen.insert(db.alias.clone(), ()) {
            let _ = prev;
            bail!("alias duplicado em databases.yaml: '{}'", db.alias);
        }
    }

    if let Some(default) = &file.default {
        if !file.databases.iter().any(|d| &d.alias == default) {
            bail!("databases.yaml: default '{}' nao corresponde a nenhum alias", default);
        }
    }

    Ok(file)
}

fn is_valid_password_ref(s: &str) -> bool {
    s.starts_with("env://") || s.starts_with("k8s-secret://") || s.starts_with("vault://")
}

fn unique_alias(existing: &[DatabaseEntry], desired: &str) -> String {
    if !existing.iter().any(|d| d.alias == desired) {
        return desired.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{}-{}", desired, n);
        if !existing.iter().any(|d| d.alias == candidate) {
            return candidate;
        }
    }
    format!("{}-{}", desired, uuid_like_suffix())
}

fn uuid_like_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

async fn persist_if_needed(state: &RegistryState) -> Result<()> {
    if !state.persistent {
        return Ok(());
    }
    let path = state
        .path
        .as_ref()
        .ok_or_else(|| anyhow!("registry persistente sem path definido"))?;

    let yaml = serde_yaml::to_string(&state.file)
        .context("falha serializando databases.yaml")?;

    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, yaml)
        .await
        .with_context(|| format!("falha escrevendo {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .await
        .with_context(|| format!("falha renomeando {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}
