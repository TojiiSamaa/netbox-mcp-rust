//! Registre des types d'objets : la table du serveur Python (types de base) + ce que NetBox annonce
//! (`core/object-types`) : nouveautés de version et modèles de plugins, ajoutés à chaud.

use crate::client::NetBox;
use crate::error::{ErrorType, NbError, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::RwLock;

#[derive(Clone, Debug, PartialEq)]
pub struct TypeInfo {
    pub key: String,
    pub name: String,
    /// Chemin relatif à /api, sans barres (`dcim/devices`).
    pub endpoint: String,
    pub fallback: Option<String>,
    pub plugin: bool,
}

#[derive(Deserialize)]
struct Raw {
    name: String,
    endpoint: String,
    fallback: Option<String>,
}

pub struct TypeRegistry {
    map: RwLock<BTreeMap<String, TypeInfo>>,
}

/// `/api/plugins/netbox-dns/zones/` ou `/netbox/api/dcim/sites/` (NetBox sous un sous-chemin) → `plugins/netbox-dns/zones`.
/// Le serveur Python ne retirait que `api/` en tête : derrière un préfixe d'URL toute la découverte échouait.
pub fn endpoint_from_rest_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let after = match path.find("/api/") {
        Some(i) => &path[i + 5..],
        None => path.strip_prefix("api/")?,
    };
    let p = after.trim_matches('/');
    (!p.is_empty()).then(|| p.to_string())
}

impl TypeRegistry {
    pub fn builtin() -> Self {
        let raw: BTreeMap<String, Raw> = serde_json::from_str(include_str!("../types.json")).expect("types.json invalide");
        let map = raw
            .into_iter()
            .map(|(key, r)| (key.clone(), TypeInfo { key, name: r.name, endpoint: r.endpoint, fallback: r.fallback, plugin: false }))
            .collect();
        TypeRegistry { map: RwLock::new(map) }
    }

    pub fn keys(&self) -> Vec<String> {
        self.map.read().unwrap().keys().cloned().collect()
    }

    pub fn all(&self) -> Vec<TypeInfo> {
        self.map.read().unwrap().values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Cherche un type : clé exacte (`dcim.device`), puis forme `dcim/devices`, puis nom de modèle seul
    /// (`device`) s'il est sans ambiguïté. Sinon erreur avec des propositions.
    pub fn resolve(&self, wanted: &str) -> Result<TypeInfo> {
        let map = self.map.read().unwrap();
        let w = wanted.trim().to_ascii_lowercase();
        if let Some(t) = map.get(&w) {
            return Ok(t.clone());
        }
        let as_path = w.trim_matches('/');
        let as_path = as_path.strip_prefix("api/").unwrap_or(as_path);
        if let Some(t) = map.values().find(|t| t.endpoint == as_path) {
            return Ok(t.clone());
        }
        let model = w.rsplit('.').next().unwrap_or(&w);
        let same_model: Vec<&TypeInfo> = map.values().filter(|t| t.key.rsplit('.').next() == Some(model)).collect();
        if same_model.len() == 1 && !w.contains('.') {
            return Ok(same_model[0].clone());
        }
        let mut near: Vec<&str> = map.keys().filter(|k| k.contains(model) || model.contains(k.rsplit('.').next().unwrap_or(""))).map(String::as_str).take(8).collect();
        near.sort_unstable();
        let mut e = NbError::new(ErrorType::InvalidInput, format!("Unknown object_type \"{wanted}\"."));
        e = e.hint(if near.is_empty() {
            "Call netbox_list_object_types for the valid values (core and plugin types).".to_string()
        } else {
            format!("Did you mean: {}? netbox_list_object_types lists them all.", near.join(", "))
        });
        Err(e)
    }

    /// Ajoute ce que NetBox annonce. Un type déjà connu garde son entrée (ses replis de version).
    pub async fn discover(&self, nb: &NetBox) -> Result<usize> {
        let mut found: Vec<TypeInfo> = Vec::new();
        let mut offset = 0;
        loop {
            let q = [("limit".to_string(), "100".to_string()), ("offset".to_string(), offset.to_string())];
            let page = nb.get_with_fallback("core/object-types", Some("extras/object-types"), &q).await?;
            let results = page.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
            for t in &results {
                let (Some(app), Some(model)) = (t.get("app_label").and_then(Value::as_str), t.get("model").and_then(Value::as_str)) else { continue };
                let Some(endpoint) = t.get("rest_api_endpoint").and_then(Value::as_str).and_then(endpoint_from_rest_url) else { continue };
                found.push(TypeInfo {
                    key: format!("{app}.{model}"),
                    name: t.get("display").and_then(Value::as_str).unwrap_or(model).to_string(),
                    endpoint,
                    fallback: None,
                    plugin: t.get("is_plugin_model").and_then(Value::as_bool).unwrap_or(false),
                });
            }
            if page.get("next").map_or(true, Value::is_null) || results.is_empty() {
                break;
            }
            offset += 100;
        }
        let mut map = self.map.write().unwrap();
        let mut added = 0;
        for t in found {
            match map.get_mut(&t.key) {
                // Le chemin annoncé par NetBox est le bon pour cette version : il remplace celui de la table.
                Some(known) => {
                    known.endpoint = t.endpoint;
                    known.plugin = t.plugin;
                }
                None => {
                    map.insert(t.key.clone(), t);
                    added += 1;
                }
            }
        }
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_loads() {
        let r = TypeRegistry::builtin();
        assert!(r.len() > 100);
        assert_eq!(r.resolve("dcim.device").unwrap().endpoint, "dcim/devices");
    }

    #[test]
    fn lenient_lookup() {
        let r = TypeRegistry::builtin();
        assert_eq!(r.resolve("DCIM.Device").unwrap().key, "dcim.device");
        assert_eq!(r.resolve("device").unwrap().key, "dcim.device");
        assert_eq!(r.resolve("dcim/devices/").unwrap().key, "dcim.device");
        assert_eq!(r.resolve("/api/ipam/prefixes/").unwrap().key, "ipam.prefix");
        let e = r.resolve("dcim.devise").unwrap_err();
        assert!(e.is(ErrorType::InvalidInput));
    }

    #[test]
    fn rest_url_with_prefix_and_plugins() {
        assert_eq!(endpoint_from_rest_url("/api/plugins/netbox-dns/zones/").unwrap(), "plugins/netbox-dns/zones");
        assert_eq!(endpoint_from_rest_url("/netbox/api/dcim/sites/").unwrap(), "dcim/sites");
        assert_eq!(endpoint_from_rest_url("https://h/api/dcim/sites/?x=1").unwrap(), "dcim/sites");
        assert!(endpoint_from_rest_url("/nothing/").is_none());
        assert!(endpoint_from_rest_url("/api/").is_none());
    }
}
