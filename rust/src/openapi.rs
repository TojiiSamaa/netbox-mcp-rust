//! Lecture du schéma OpenAPI de NetBox (`/api/schema/`) : champs d'un type, filtres acceptés, et actions
//! hors CRUD (trace, render-config, available-ips, actions des plugins...). C'est ce qui rend les plugins
//! « appelables » sans rien coder : tout endpoint qu'ils déclarent apparaît ici.

use serde_json::{json, Map, Value};

/// Suit les `$ref` (`#/components/schemas/X`) d'un schéma, quelques sauts au plus.
fn resolve<'a>(root: &'a Value, mut v: &'a Value) -> &'a Value {
    for _ in 0..6 {
        let Some(r) = v.get("$ref").and_then(Value::as_str) else { break };
        let Some(target) = r.strip_prefix('#').and_then(|p| root.pointer(p)) else { break };
        v = target;
    }
    v
}

fn short_type(root: &Value, prop: &Value) -> (String, Option<Value>, bool) {
    let r = resolve(root, prop);
    // `allOf`/`oneOf` enveloppent un $ref (clé étrangère, énumération) : on lit le premier membre.
    let r = ["allOf", "oneOf", "anyOf"]
        .iter()
        .find_map(|k| r.get(*k).and_then(Value::as_array).and_then(|a| a.first()))
        .map(|m| resolve(root, m))
        .unwrap_or(r);
    let nullable = prop.get("nullable").and_then(Value::as_bool).unwrap_or(false) || r.get("nullable").and_then(Value::as_bool).unwrap_or(false);
    let ty = match r.get("type").and_then(Value::as_str) {
        Some("array") => {
            let inner = r.get("items").map(|i| short_type(root, i).0).unwrap_or_else(|| "any".into());
            format!("array<{inner}>")
        }
        Some("object") if r.get("properties").is_some() => "object (id, or a nested object with a unique lookup such as {\"name\": ...})".to_string(),
        Some(t) => match r.get("format").and_then(Value::as_str) {
            Some(f) => format!("{t} ({f})"),
            None => t.to_string(),
        },
        None if r.get("enum").is_some() => "string".into(),
        None => "any".into(),
    };
    (ty, r.get("enum").cloned(), nullable)
}

/// Les schémas objet (avec `properties`) d'un corps de requête, à travers `$ref`, `allOf`, `oneOf` et `anyOf`.
/// Les membres tableau (`type: array`) sont ignorés : seul l'objet décrit les champs.
fn object_members<'a>(root: &'a Value, v: &'a Value, out: &mut Vec<&'a Value>, depth: usize) {
    let r = resolve(root, v);
    if r.get("properties").is_some() {
        out.push(r);
    }
    if depth >= 4 {
        return;
    }
    for k in ["allOf", "oneOf", "anyOf"] {
        for m in r.get(k).and_then(Value::as_array).into_iter().flatten() {
            object_members(root, m, out, depth + 1);
        }
    }
}

fn api_tail(path: &str) -> &str {
    path.find("/api/").map_or(path, |i| &path[i + 4..])
}

/// Description d'un endpoint de liste (`dcim/devices`) : champs écrivables, filtres, actions.
pub fn describe(schema: &Value, endpoint: &str) -> Option<Value> {
    let paths = schema.get("paths")?.as_object()?;
    let list_key = paths.keys().find(|k| api_tail(k).trim_matches('/') == endpoint)?;
    let list = &paths[list_key];
    let detail_prefix = format!("{}{{", api_tail(list_key));

    // NetBox 4 déclare le POST de liste en `oneOf: [objet, tableau d'objets]` (création en lot) :
    // les champs sont dans le membre objet, pas à la racine. Repli sur le PATCH du détail si le POST n'en donne pas.
    let post = list.pointer("/post/requestBody/content/application~1json/schema");
    let patch = paths
        .iter()
        .find(|(k, _)| api_tail(k).starts_with(&detail_prefix) && api_tail(k).matches('/').count() == api_tail(list_key).matches('/').count() + 1)
        .and_then(|(_, d)| d.pointer("/patch/requestBody/content/application~1json/schema"));
    let objs = [post, patch]
        .into_iter()
        .flatten()
        .map(|b| {
            let mut found = Vec::new();
            object_members(schema, b, &mut found, 0);
            found
        })
        .find(|f| !f.is_empty())
        .unwrap_or_default();
    let required: Vec<&str> = objs.iter().flat_map(|o| o.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)).collect();
    let mut fields = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for props in objs.iter().filter_map(|o| o.get("properties").and_then(Value::as_object)) {
        for (name, p) in props {
            if !seen.insert(name.as_str()) {
                continue;
            }
            let (ty, choices, nullable) = short_type(schema, p);
            let rp = resolve(schema, p);
            let mut f = Map::new();
            f.insert("name".into(), json!(name));
            f.insert("type".into(), json!(ty));
            f.insert("required".into(), json!(required.contains(&name.as_str())));
            if nullable {
                f.insert("nullable".into(), json!(true));
            }
            if rp.get("readOnly").and_then(Value::as_bool) == Some(true) {
                f.insert("read_only".into(), json!(true));
            }
            if let Some(c) = choices {
                f.insert("choices".into(), c);
            }
            if let Some(d) = p.get("description").or_else(|| rp.get("description")).and_then(Value::as_str).filter(|d| !d.is_empty()) {
                f.insert("description".into(), json!(d.chars().take(160).collect::<String>()));
            }
            fields.push(Value::Object(f));
        }
    }

    let filters: Vec<&str> = list
        .pointer("/get/parameters")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|p| p.get("in").and_then(Value::as_str) == Some("query")).filter_map(|p| p.get("name").and_then(Value::as_str)).collect())
        .unwrap_or_default();

    let mut actions = Vec::new();
    let prefix = format!("{}{{", api_tail(list_key));
    let depth = api_tail(list_key).matches('/').count();
    for (k, v) in paths {
        let t = api_tail(k);
        if !t.starts_with(&prefix) || t.matches('/').count() <= depth + 1 {
            continue; // le détail `{id}/` lui-même n'est pas une action
        }
        let methods: Vec<String> = v.as_object().into_iter().flatten().filter(|(m, _)| ["get", "post", "put", "patch", "delete"].contains(&m.as_str())).map(|(m, _)| m.to_ascii_uppercase()).collect();
        let summary = v.as_object().into_iter().flatten().find_map(|(_, op)| op.get("summary").and_then(Value::as_str));
        actions.push(json!({ "path": t.trim_matches('/'), "methods": methods, "summary": summary }));
    }

    Some(json!({
        "endpoint": endpoint,
        "methods": list.as_object().map(|o| o.keys().map(|m| m.to_ascii_uppercase()).collect::<Vec<_>>()),
        "fields": fields,
        "filters": filters,
        "actions": actions,
        "note": "Foreign keys take the numeric id (or a nested object with a unique lookup). Read-only fields are ignored on write. Use `filters` names as keys of `filters`.",
    }))
}

/// Les endpoints de liste du schéma qui ne sont pas dans le registre : plugins sans ContentType, etc.
pub fn list_endpoints(schema: &Value) -> Vec<String> {
    let Some(paths) = schema.get("paths").and_then(Value::as_object) else { return vec![] };
    paths
        .iter()
        .filter(|(k, v)| v.get("get").is_some() && !api_tail(k).contains('{'))
        .map(|(k, _)| api_tail(k).trim_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({
          "paths": {
            "/api/dcim/sites/": {
              "get": { "parameters": [ {"name": "name", "in": "query"}, {"name": "id", "in": "path"} ] },
              "post": { "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SiteReq" } } } } }
            },
            "/api/dcim/sites/{id}/": { "get": {}, "patch": {}, "delete": {} },
            "/api/dcim/sites/{id}/thing/": { "get": { "summary": "Do thing" } }
          },
          "components": { "schemas": {
            "SiteReq": { "type": "object", "required": ["name"], "properties": {
              "name": { "type": "string", "description": "Site name" },
              "status": { "$ref": "#/components/schemas/StatusEnum" },
              "region": { "allOf": [ {"$ref": "#/components/schemas/Brief"} ], "nullable": true },
              "tags": { "type": "array", "items": { "type": "integer" } }
            }},
            "StatusEnum": { "type": "string", "enum": ["active", "planned"] },
            "Brief": { "type": "object", "properties": {"id": {"type": "integer"}} }
          }}
        })
    }

    #[test]
    fn describes_fields_filters_actions() {
        let d = describe(&schema(), "dcim/sites").unwrap();
        let fields = d["fields"].as_array().unwrap();
        let by = |n: &str| fields.iter().find(|f| f["name"] == n).unwrap();
        assert_eq!(by("name")["required"], true);
        assert_eq!(by("status")["choices"], json!(["active", "planned"]));
        assert_eq!(by("region")["nullable"], true);
        assert!(by("region")["type"].as_str().unwrap().starts_with("object"));
        assert_eq!(by("tags")["type"], "array<integer>");
        assert_eq!(d["filters"], json!(["name"]));
        assert_eq!(d["actions"].as_array().unwrap().len(), 1);
        assert_eq!(d["actions"][0]["path"], "dcim/sites/{id}/thing");
    }

    #[test]
    fn describes_a_body_declared_as_object_or_array() {
        // NetBox 4 : POST de liste = oneOf [objet, tableau d'objets].
        let s = json!({
          "paths": { "/api/dcim/sites/": { "get": {}, "post": { "requestBody": { "content": { "application/json": { "schema": {
              "oneOf": [ {"$ref": "#/components/schemas/WritableSiteRequest"}, {"type": "array", "items": {"$ref": "#/components/schemas/WritableSiteRequest"}} ] } } } } } } },
          "components": { "schemas": { "WritableSiteRequest": { "type": "object", "required": ["name", "slug"],
              "properties": { "name": {"type": "string"}, "slug": {"type": "string"} } } } }
        });
        let d = describe(&s, "dcim/sites").unwrap();
        let names: Vec<&str> = d["fields"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["name", "slug"]);
        assert_eq!(d["fields"][1]["required"], true);
    }

    #[test]
    fn unknown_endpoint_is_none() {
        assert!(describe(&schema(), "dcim/nope").is_none());
    }

    #[test]
    fn lists_collection_endpoints_only() {
        assert_eq!(list_endpoints(&schema()), vec!["dcim/sites".to_string()]);
    }
}
