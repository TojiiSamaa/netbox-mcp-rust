//! NetBox simulé du mode test. Il répond à la place du réseau : les fixtures (`test/fixtures/netbox.json`)
//! donnent les documents fixes, le reste (listes, objets, écritures) est déduit du chemin et du corps.
//! Rien ici ne touche au réseau, au disque ni à un processus.

use reqwest::Method;
use serde_json::{json, Value};

pub struct Mock {
    fx: Value,
}

/// Réponse simulée : code HTTP et corps JSON (`None` = 204).
pub type Reply = (u16, Option<Value>);

fn numeric(s: &str) -> Option<i64> {
    s.parse().ok().filter(|n| *n >= 1)
}

fn page(rows: Vec<Value>) -> Value {
    json!({ "count": rows.len(), "next": null, "previous": null, "results": rows })
}

impl Mock {
    pub fn new() -> Self {
        Mock { fx: serde_json::from_str(include_str!("../test/fixtures/netbox.json")).expect("test/fixtures/netbox.json is valid JSON") }
    }

    fn fixture(&self, key: &str) -> Value {
        self.fx.get(key).cloned().unwrap_or(Value::Null)
    }

    /// Un objet de la fixture, avec l'id demandé.
    fn object(&self, id: i64) -> Value {
        let mut o = self.fixture("object");
        o["id"] = json!(id);
        o["display"] = json!(format!("test-object-{id}"));
        o["name"] = json!(format!("test-object-{id}"));
        o
    }

    /// L'objet de la fixture, surchargé par le corps reçu (création ou mise à jour simulée).
    fn merged(&self, body: &Value, id: i64) -> Value {
        let mut o = self.object(id);
        if let (Some(m), Some(b)) = (o.as_object_mut(), body.as_object()) {
            m.extend(b.clone());
            m.insert("id".into(), json!(id));
        }
        o
    }

    fn created(&self, body: Option<&Value>) -> Reply {
        match body {
            Some(Value::Array(a)) => (201, Some(Value::Array(a.iter().enumerate().map(|(i, b)| self.merged(b, 1001 + i as i64)).collect()))),
            Some(b) => (201, Some(self.merged(b, 1001))),
            None => (400, Some(json!({ "detail": "empty body" }))),
        }
    }

    /// `path` : relatif à `/api` (`dcim/devices/12`) ou `graphql`.
    pub fn reply(&self, method: &Method, path: &str, query: &[(String, String)], body: Option<&Value>) -> Reply {
        let path = path.trim_matches('/');
        let segs: Vec<&str> = path.split('/').collect();
        let last = *segs.last().unwrap_or(&"");
        let m = method.as_str();
        match (m, path) {
            ("POST", "graphql") => return (200, Some(self.fixture("graphql"))),
            ("GET", "status") => return (200, Some(self.fixture("status"))),
            ("GET", "schema") => return (200, Some(self.fixture("schema"))),
            ("GET", "core/object-types") => return (200, Some(self.fixture("object_types"))),
            ("GET", "core/object-changes") => return (200, Some(self.fixture("changes"))),
            ("GET", "extras/scripts") => return (200, Some(page(vec![json!({ "id": 3, "name": "test-script", "display": "test-script" })]))),
            _ => {}
        }
        match (m, last) {
            ("GET", "trace") => return (200, Some(self.fixture("trace"))),
            ("GET", "elevation") => return (200, Some(self.fixture("elevation"))),
            ("POST", "render-config") => return (200, Some(self.fixture("render_config"))),
            _ => {}
        }
        if m == "POST" && segs.len() == 3 && segs[0] == "extras" && segs[1] == "scripts" {
            return (200, Some(self.fixture("script_result")));
        }
        if last.starts_with("available-") {
            return match m {
                "GET" => (200, Some(json!([{ "family": 4, "address": "10.0.0.5/24", "vrf": null }, { "family": 4, "address": "10.0.0.6/24", "vrf": null }]))),
                "POST" => self.created(body),
                _ => (405, Some(json!({ "detail": "Method not allowed." }))),
            };
        }
        match (m, numeric(last)) {
            ("GET", Some(id)) => (200, Some(self.object(id))),
            ("PATCH" | "PUT", Some(id)) => (200, Some(self.merged(body.unwrap_or(&Value::Null), id))),
            ("DELETE", _) => (204, None),
            ("GET", None) => {
                // `id` répété : la liste contient exactement ces objets (les écritures en lot vérifient leur existence).
                let ids: Vec<i64> = query.iter().filter(|(k, _)| k == "id").filter_map(|(_, v)| v.parse().ok()).collect();
                let ids = if ids.is_empty() { vec![1, 2] } else { ids };
                (200, Some(page(ids.into_iter().map(|i| self.object(i)).collect())))
            }
            ("POST", None) => self.created(body),
            ("PATCH" | "PUT", None) => match body {
                Some(Value::Array(a)) => (200, Some(Value::Array(a.iter().map(|b| self.merged(b, b.get("id").and_then(Value::as_i64).unwrap_or(1))).collect()))),
                _ => (400, Some(json!({ "detail": "a list is expected" }))),
            },
            _ => (404, Some(json!({ "detail": "Not found." }))),
        }
    }
}

impl Default for Mock {
    fn default() -> Self {
        Self::new()
    }
}
