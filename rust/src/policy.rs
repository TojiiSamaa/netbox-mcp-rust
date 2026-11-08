//! Qui peut écrire quoi. Les écritures sont fermées par défaut ; ici on ne règle que la portée :
//! liste blanche/noire de types, et refus de `users.*` (jetons, droits) sauf demande explicite.

use crate::config::Config;
use crate::error::{ErrorType, NbError, Result};
use crate::types::TypeRegistry;

/// `*` : tout sauf `users.*`. `dcim.*` : une application. Sinon la clé exacte.
pub fn pattern_matches(pattern: &str, key: &str) -> bool {
    if pattern == "*" {
        return !key.starts_with("users.");
    }
    match pattern.strip_suffix(".*") {
        Some(app) => key.split('.').next() == Some(app),
        None => pattern == key,
    }
}

fn denied(key: &str, why: &str) -> NbError {
    NbError::new(ErrorType::TypeNotAllowed, format!("Writing {key} is not allowed: {why}."))
        .hint("Do not retry or look for another route. The operator can widen NETBOX_WRITE_TYPES / narrow NETBOX_WRITE_DENY.")
}

/// Le type `key` (`dcim.device`) est-il écrivable avec cette configuration ?
pub fn check_type_writable(cfg: &Config, key: &str) -> Result<()> {
    if cfg.write_deny.iter().any(|p| pattern_matches(p, key)) {
        return Err(denied(key, "it is in NETBOX_WRITE_DENY"));
    }
    if !cfg.write_allow.iter().any(|p| pattern_matches(p, key)) {
        let why = if key.starts_with("users.") { "user, token and permission objects need an explicit users.* entry in NETBOX_WRITE_TYPES" } else { "it is not in NETBOX_WRITE_TYPES" };
        return Err(denied(key, why));
    }
    Ok(())
}

/// Écriture sur un chemin libre : on retrouve le type par son endpoint (le plus long préfixe). Un chemin
/// sans type connu (scripts, jobs...) n'est accepté que si l'écriture n'est pas restreinte à une liste.
pub fn check_path_writable(cfg: &Config, types: &TypeRegistry, path: &str) -> Result<()> {
    let matched = types
        .all()
        .into_iter()
        .filter(|t| path == t.endpoint || path.starts_with(&format!("{}/", t.endpoint)))
        .max_by_key(|t| t.endpoint.len());
    if let Some(t) = matched {
        return check_type_writable(cfg, &t.key);
    }
    let app = path.split('/').next().unwrap_or("");
    if cfg.write_deny.iter().any(|p| pattern_matches(p, &format!("{app}.x"))) {
        return Err(denied(path, "its application is in NETBOX_WRITE_DENY"));
    }
    if app == "users" && !cfg.write_allow.iter().any(|p| p == "users.*") {
        return Err(denied(path, "user, token and permission endpoints need an explicit users.* entry in NETBOX_WRITE_TYPES"));
    }
    if cfg.write_allow.iter().any(|p| p == "*") {
        return Ok(());
    }
    Err(denied(path, "this path is not a known object type and writes are limited to a list of types"))
}

pub fn check_writes_on(cfg: &Config, is_delete: bool) -> Result<()> {
    if !cfg.allow_writes && !cfg.dry_run {
        return Err(NbError::new(ErrorType::WriteDisabled, "This server is read-only: nothing was sent to NetBox.")
            .hint("Do not retry. The operator can enable writes with NETBOX_ALLOW_WRITES=true, or preview with NETBOX_DRY_RUN=true."));
    }
    if is_delete && !cfg.allow_delete && !cfg.dry_run {
        return Err(NbError::new(ErrorType::DeleteDisabled, "Deletions are disabled: nothing was sent to NetBox.")
            .hint("Do not retry. The operator can enable them with NETBOX_ALLOW_DELETE=true."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{load_config, Env};

    fn cfg(extra: &[(&str, &str)]) -> Config {
        let mut e: Env = [("NETBOX_URL", "http://n"), ("NETBOX_TOKEN", "t")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        e.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        load_config(&e).unwrap()
    }

    #[test]
    fn star_excludes_users() {
        assert!(pattern_matches("*", "dcim.device"));
        assert!(!pattern_matches("*", "users.token"));
        assert!(pattern_matches("users.*", "users.token"));
        assert!(pattern_matches("dcim.*", "dcim.device") && !pattern_matches("dcim.*", "ipam.vlan"));
    }

    #[test]
    fn allow_and_deny() {
        let c = cfg(&[("NETBOX_WRITE_TYPES", "dcim.*,ipam.vlan"), ("NETBOX_WRITE_DENY", "dcim.cable")]);
        assert!(check_type_writable(&c, "dcim.device").is_ok());
        assert!(check_type_writable(&c, "ipam.vlan").is_ok());
        assert!(check_type_writable(&c, "dcim.cable").is_err());
        assert!(check_type_writable(&c, "ipam.prefix").is_err());
        assert!(check_type_writable(&cfg(&[]), "users.token").is_err());
    }

    #[test]
    fn paths_resolve_to_types() {
        let t = TypeRegistry::builtin();
        let c = cfg(&[("NETBOX_WRITE_DENY", "dcim.cable")]);
        assert!(check_path_writable(&c, &t, "dcim/devices/3").is_ok());
        assert!(check_path_writable(&c, &t, "dcim/cables/3").is_err());
        assert!(check_path_writable(&c, &t, "users/tokens").is_err());
        assert!(check_path_writable(&c, &t, "extras/scripts/5").is_ok());
        let narrow = cfg(&[("NETBOX_WRITE_TYPES", "dcim.device")]);
        assert!(check_path_writable(&narrow, &t, "extras/scripts/5").is_err());
    }

    #[test]
    fn switches() {
        assert!(check_writes_on(&cfg(&[]), false).is_err());
        assert!(check_writes_on(&cfg(&[("NETBOX_ALLOW_WRITES", "true")]), false).is_ok());
        assert!(check_writes_on(&cfg(&[("NETBOX_ALLOW_WRITES", "true")]), true).is_err());
        assert!(check_writes_on(&cfg(&[("NETBOX_ALLOW_WRITES", "true"), ("NETBOX_ALLOW_DELETE", "true")]), true).is_ok());
        assert!(check_writes_on(&cfg(&[("NETBOX_DRY_RUN", "true")]), true).is_ok());
    }
}
