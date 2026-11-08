//! Configuration : variables d'environnement (mêmes noms que le serveur Python, plus celles de l'écriture).
//! Priorité : options de ligne de commande > environnement > fichier `.env`.

use crate::error::{ErrorType, NbError, Result};
use std::collections::{HashMap, HashSet};

pub type Env = HashMap<String, String>;

/// Quand demander une confirmation humaine (si le client sait demander) : jamais, avant une suppression
/// ou un script (défaut), ou avant toute écriture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elicit {
    Off,
    Destructive,
    All,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub netbox_url: String,
    /// Jeton NetBox du serveur. Absent seulement avec `passthrough` (chaque client apporte le sien).
    pub token: Option<String>,
    pub verify_ssl: bool,
    pub timeout_secs: u64,
    pub allow_writes: bool,
    pub allow_delete: bool,
    pub dry_run: bool,
    pub elicit: Elicit,
    /// Motifs de types écrivables (`dcim.*`, `netbox_dns.zone`, `*`). `users.*` n'est jamais inclus par `*`.
    pub write_allow: Vec<String>,
    pub write_deny: Vec<String>,
    pub max_bulk: usize,
    pub max_limit: i64,
    pub max_response_chars: usize,
    pub redact: bool,
    pub plugin_discovery: bool,
    pub audit_log: Option<String>,
    pub disabled_tools: HashSet<String>,
    pub passthrough: bool,
    pub mcp_token: Option<String>,
    pub cors_origins: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub log_level: String,
}

pub fn process_env() -> Env {
    std::env::vars().collect()
}

/// Lit un `.env` simple (`CLE=valeur`, `#` commentaire, guillemets facultatifs). Ne remplace rien d'existant.
pub fn merge_dotenv(env: &mut Env, text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        env.entry(k.trim().trim_start_matches("export ").to_string()).or_insert_with(|| v.to_string());
    }
}

fn flag(env: &Env, key: &str, default: bool) -> Result<bool> {
    match env.get(key).map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(default),
        Some(v) if v.is_empty() => Ok(default),
        Some(v) if ["1", "true", "yes", "on"].contains(&v.as_str()) => Ok(true),
        Some(v) if ["0", "false", "no", "off"].contains(&v.as_str()) => Ok(false),
        Some(v) => Err(NbError::new(ErrorType::ConfigError, format!("{key}: \"{v}\" is not a boolean (true/false)."))),
    }
}

fn number<T: std::str::FromStr>(env: &Env, key: &str, default: T) -> Result<T> {
    match env.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| NbError::new(ErrorType::ConfigError, format!("{key}: \"{v}\" is not a valid number."))),
    }
}

fn list(env: &Env, key: &str) -> Vec<String> {
    env.get(key)
        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn text(env: &Env, key: &str) -> Option<String> {
    env.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn load_config(env: &Env) -> Result<Config> {
    let passthrough = flag(env, "NETBOX_TOKEN_PASSTHROUGH", false)?;
    let netbox_url = text(env, "NETBOX_URL").ok_or_else(|| {
        NbError::new(ErrorType::ConfigError, "NETBOX_URL is not set.").hint("Example: NETBOX_URL=https://netbox.example.com/")
    })?;
    if !(netbox_url.starts_with("http://") || netbox_url.starts_with("https://")) {
        return Err(NbError::new(ErrorType::ConfigError, format!("NETBOX_URL: \"{netbox_url}\" must start with http:// or https://.")));
    }
    let token = text(env, "NETBOX_TOKEN");
    if token.is_none() && !passthrough {
        return Err(NbError::new(ErrorType::ConfigError, "NETBOX_TOKEN is not set.")
            .hint("Create an API token in NetBox, or set NETBOX_TOKEN_PASSTHROUGH=true so each HTTP client brings its own."));
    }
    let allow_writes = flag(env, "NETBOX_ALLOW_WRITES", false)?;
    let mut write_allow = list(env, "NETBOX_WRITE_TYPES");
    if write_allow.is_empty() {
        write_allow.push("*".into());
    }
    Ok(Config {
        netbox_url: netbox_url.trim_end_matches('/').to_string(),
        token,
        verify_ssl: flag(env, "VERIFY_SSL", true)?,
        timeout_secs: number(env, "NETBOX_TIMEOUT", 30)?,
        allow_writes,
        allow_delete: flag(env, "NETBOX_ALLOW_DELETE", false)?,
        dry_run: flag(env, "NETBOX_DRY_RUN", false)?,
        elicit: match text(env, "NETBOX_ELICIT").map(|v| v.to_ascii_lowercase()).as_deref() {
            None | Some("true") | Some("destructive") => Elicit::Destructive,
            Some("false") | Some("off") | Some("0") => Elicit::Off,
            Some("all") => Elicit::All,
            Some(v) => return Err(NbError::new(ErrorType::ConfigError, format!("NETBOX_ELICIT: \"{v}\" is not valid (off, destructive, all)."))),
        },
        write_allow,
        write_deny: list(env, "NETBOX_WRITE_DENY"),
        max_bulk: number(env, "NETBOX_MAX_BULK", 100)?,
        max_limit: number(env, "NETBOX_MAX_LIMIT", 100)?,
        max_response_chars: number(env, "NETBOX_MAX_RESPONSE_CHARS", 60_000)?,
        redact: flag(env, "NETBOX_REDACT_SECRETS", true)?,
        plugin_discovery: flag(env, "ENABLE_PLUGIN_DISCOVERY", true)?,
        audit_log: text(env, "NETBOX_AUDIT_LOG"),
        disabled_tools: list(env, "NETBOX_DISABLED_TOOLS").into_iter().collect(),
        passthrough,
        mcp_token: text(env, "NETBOX_MCP_TOKEN").or_else(|| text(env, "MCP_AUTH_TOKEN")),
        cors_origins: list(env, "CORS_ORIGINS"),
        allowed_hosts: list(env, "NETBOX_MCP_ALLOWED_HOSTS"),
        log_level: text(env, "LOG_LEVEL").unwrap_or_else(|| "INFO".into()).to_ascii_uppercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn needs_url_and_token() {
        assert!(load_config(&env(&[])).is_err());
        assert!(load_config(&env(&[("NETBOX_URL", "https://n/")])).is_err());
        let c = load_config(&env(&[("NETBOX_URL", "https://n/"), ("NETBOX_TOKEN", "t")])).unwrap();
        assert_eq!(c.netbox_url, "https://n");
        assert!(!c.allow_writes && !c.allow_delete && c.verify_ssl && c.plugin_discovery);
    }

    #[test]
    fn passthrough_needs_no_token() {
        let c = load_config(&env(&[("NETBOX_URL", "http://n"), ("NETBOX_TOKEN_PASSTHROUGH", "true")])).unwrap();
        assert!(c.passthrough && c.token.is_none());
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(load_config(&env(&[("NETBOX_URL", "n"), ("NETBOX_TOKEN", "t")])).is_err());
        assert!(load_config(&env(&[("NETBOX_URL", "http://n"), ("NETBOX_TOKEN", "t"), ("NETBOX_ALLOW_WRITES", "maybe")])).is_err());
    }

    #[test]
    fn dotenv_never_overrides() {
        let mut e = env(&[("A", "1")]);
        merge_dotenv(&mut e, "# c\nA=2\nexport B=\"x y\"\n");
        assert_eq!(e["A"], "1");
        assert_eq!(e["B"], "x y");
    }
}
