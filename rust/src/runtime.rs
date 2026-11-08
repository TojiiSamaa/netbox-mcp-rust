//! Ce qui survit à une connexion MCP : client NetBox, registre de types, schéma OpenAPI (chargé une fois),
//! journal d'audit.

use crate::client::NetBox;
use crate::config::Config;
use crate::error::{ErrorType, NbError, Result};
use crate::stats::Stats;
use crate::types::TypeRegistry;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::sync::Arc;
use tokio::sync::OnceCell;

pub struct Runtime {
    pub cfg: Arc<Config>,
    pub nb: NetBox,
    pub stats: Arc<Stats>,
    pub types: Arc<TypeRegistry>,
    schema: OnceCell<Value>,
}

impl Runtime {
    /// Construit le client et, si demandé, découvre les types annoncés par NetBox (échec toléré : on garde la table de base).
    pub async fn new(cfg: Config) -> Result<Arc<Self>> {
        if let Some(path) = &cfg.audit_log {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| NbError::new(ErrorType::ConfigError, format!("NETBOX_AUDIT_LOG: cannot write to {path}: {e}")))?;
        }
        let stats = Arc::new(Stats::new());
        let nb = NetBox::new(&cfg, stats.clone())?;
        let types = Arc::new(TypeRegistry::builtin());
        if cfg.plugin_discovery && cfg.token.is_some() {
            match types.discover(&nb).await {
                Ok(n) => crate::log::info(&format!("type discovery: {n} type(s) added, {} known", types.len())),
                Err(e) => crate::log::warn(&format!("type discovery failed, using the built-in table: {}", e.message)),
            }
        }
        Ok(Arc::new(Runtime { cfg: Arc::new(cfg), nb, stats, types, schema: OnceCell::new() }))
    }

    /// Schéma OpenAPI de NetBox, téléchargé à la première demande (plusieurs Mo).
    pub async fn schema(&self, nb: &NetBox) -> Result<&Value> {
        self.schema
            .get_or_try_init(|| async { nb.get("schema", &[("format".to_string(), "json".to_string())]).await })
            .await
    }

    pub fn audit(&self, entry: Value) {
        let Some(path) = &self.cfg.audit_log else { return };
        let mut line = Map::new();
        line.insert("ts".into(), json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));
        if let Value::Object(o) = entry {
            line.extend(o);
        }
        let text = format!("{}\n", Value::Object(line));
        let result = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = result {
            crate::log::error(&format!("could not write the audit log: {e}"));
        }
    }
}
