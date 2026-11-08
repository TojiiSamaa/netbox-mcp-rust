//! Les outils MCP. Noms et paramètres de lecture identiques au serveur Python (`netbox_get_objects`...),
//! plus tout ce qui manquait : écriture gardée, allocation, scripts, appel brut, GraphQL, schéma des types.

use crate::client::{api_path, query_pairs};
use crate::config::Config;
use crate::error::{ErrorType, NbError, Result};
use crate::filters::{list_query, validate_filters};
use crate::policy::{check_path_writable, check_type_writable};
use crate::redact::redact;
use crate::schema::{any, boolean, enumeration, integer, string, strings, unknown_record, Field, Schema};
use crate::tool::{Access, Args, BoxFut, Confirmation, Ctx, Handler, Precheck, ToolDef};
use crate::types::{TypeInfo, TypeRegistry};
use futures::future::join_all;
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const DEFAULT_SEARCH_TYPES: [&str; 8] = [
    "dcim.device",
    "dcim.site",
    "ipam.ipaddress",
    "dcim.interface",
    "dcim.rack",
    "ipam.vlan",
    "circuits.circuit",
    "virtualization.virtualmachine",
];

const DEFAULT_LIMIT: i64 = 5;

// ---------- aides ----------

fn resolve(ctx: &Ctx, t: &str) -> Result<TypeInfo> {
    ctx.types.resolve(t)
}

fn shape(ctx: &Ctx, mut v: Value, tokens: bool) -> Value {
    if ctx.cfg.redact {
        redact(&mut v, tokens);
    }
    v
}

fn fb(t: &TypeInfo, id: Option<i64>) -> Option<String> {
    t.fallback.as_ref().map(|f| match id {
        Some(i) => format!("{f}/{i}"),
        None => f.clone(),
    })
}

fn obj_path(t: &TypeInfo, id: i64) -> String {
    format!("{}/{}", t.endpoint, id)
}

/// Garde seulement ces clés de premier niveau (réponses d'écriture plus légères).
fn pick(v: Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return v;
    }
    match v {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(k, _)| fields.contains(k)).collect()),
        Value::Array(a) => Value::Array(a.into_iter().map(|x| pick(x, fields)).collect()),
        other => other,
    }
}

fn limit_of(a: &Args, default: i64) -> i64 {
    a.int("limit").unwrap_or(default)
}

fn object_id(a: &Args) -> Result<i64> {
    a.int("object_id").filter(|i| *i >= 1).ok_or_else(|| NbError::new(ErrorType::InvalidInput, "object_id must be a positive integer."))
}

fn non_empty_object(a: &Args, key: &str) -> Result<Map<String, Value>> {
    let m = a.obj(key);
    if m.is_empty() {
        return Err(NbError::new(ErrorType::InvalidInput, format!("`{key}` must be a non-empty object.")).hint("Call netbox_describe_type for the field names."));
    }
    Ok(m)
}

fn items_of(a: &Args, ctx: &Ctx, need_id: bool) -> Result<Vec<Value>> {
    let items = a.list("items");
    if items.is_empty() {
        return Err(NbError::new(ErrorType::InvalidInput, "`items` must be a non-empty list of objects."));
    }
    if items.len() > ctx.cfg.max_bulk {
        return Err(NbError::new(ErrorType::InvalidInput, format!("{} items; the limit is {} per call (NETBOX_MAX_BULK).", items.len(), ctx.cfg.max_bulk)).hint("Split into several calls."));
    }
    for (i, it) in items.iter().enumerate() {
        let Some(o) = it.as_object().filter(|o| !o.is_empty()) else {
            return Err(NbError::new(ErrorType::InvalidInput, format!("items[{i}] must be a non-empty object.")));
        };
        if need_id && !o.get("id").and_then(Value::as_i64).is_some_and(|n| n >= 1) {
            return Err(NbError::new(ErrorType::InvalidInput, format!("items[{i}] needs a numeric `id`.")));
        }
    }
    Ok(items)
}

fn pairs(a: &Args, key: &str) -> Vec<(String, String)> {
    query_pairs(&a.obj(key))
}

/// Contrôle de portée par type : `object_type` ou `parent_type` des arguments.
fn precheck_type(cfg: &Config, types: &TypeRegistry, a: &Args) -> Result<()> {
    let key = a.str("object_type").or_else(|| a.str("parent_type")).unwrap_or("");
    let t = types.resolve(key)?;
    check_type_writable(cfg, &t.key)
}

fn precheck_path(cfg: &Config, types: &TypeRegistry, a: &Args) -> Result<()> {
    check_path_writable(cfg, types, &api_path(a.text("path"))?)
}

fn boxed<F>(f: F) -> BoxFut<Result<Value>>
where
    F: std::future::Future<Output = Result<Value>> + Send + 'static,
{
    Box::pin(f)
}

// ---------- schémas partagés ----------

fn f_type() -> Field {
    string("object_type").desc("Object type, e.g. \"dcim.device\", \"ipam.ipaddress\", or a plugin type such as \"netbox_dns.zone\". netbox_list_object_types lists them.")
}
fn f_fields() -> Field {
    strings("fields").opt().desc("Top-level fields to return. ALWAYS set this to cut tokens, e.g. [\"id\",\"name\",\"status\"]. Empty = all fields.")
}
fn f_brief() -> Field {
    boolean("brief").opt().desc("Return only a minimal representation of each object.")
}
fn f_filters() -> Field {
    unknown_record("filters").opt().desc("NetBox filters: {\"site_id\": 1, \"name__ic\": \"sw\", \"status\": [\"active\",\"planned\"]}. No \"__in\", no multi-hop (device__site_id).")
}

fn f_limit(cfg: &Config) -> Field {
    integer("limit").opt().min(1).max(cfg.max_limit).desc("Max results (default 5). Start small.")
}
fn f_offset() -> Field {
    integer("offset").opt().min(0).desc("Skip this many results (pagination).")
}

struct Spec {
    name: &'static str,
    title: &'static str,
    description: String,
    input: Vec<Field>,
    access: Access,
    idempotent: bool,
    open_world: bool,
    handler: Handler,
    precheck: Option<Precheck>,
    confirmation: Option<Confirmation>,
}

impl Spec {
    fn build(self) -> ToolDef {
        ToolDef {
            name: self.name,
            title: self.title,
            description: self.description,
            input: Schema::new(self.input),
            access: self.access,
            idempotent: self.idempotent,
            open_world: self.open_world,
            handler: self.handler,
            precheck: self.precheck,
            confirmation: self.confirmation,
        }
    }
}

fn read(name: &'static str, title: &'static str, description: impl Into<String>, input: Vec<Field>, handler: Handler) -> ToolDef {
    Spec { name, title, description: description.into(), input, access: Access::Read, idempotent: true, open_world: true, handler, precheck: None, confirmation: None }.build()
}

#[allow(clippy::too_many_arguments)]
fn write(
    name: &'static str,
    title: &'static str,
    description: impl Into<String>,
    input: Vec<Field>,
    access: Access,
    idempotent: bool,
    handler: Handler,
    precheck: Option<Precheck>,
    confirmation: Option<Confirmation>,
) -> ToolDef {
    Spec { name, title, description: description.into(), input, access, idempotent, open_world: true, handler, precheck, confirmation }.build()
}

// ---------- lecture ----------

fn get_objects(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let filters = a.obj("filters");
        validate_filters(&filters)?;
        let q = list_query(&filters, &a.strings("fields"), a.flag("brief"), limit_of(&a, DEFAULT_LIMIT), a.int("offset").unwrap_or(0), &a.string_or_list("ordering"));
        let v = ctx.nb.get_with_fallback(&t.endpoint, t.fallback.as_deref(), &q).await?;
        Ok(shape(&ctx, v, t.key == "users.token"))
    })
}

fn get_object_by_id(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let id = object_id(&a)?;
        let mut q = Vec::new();
        let fields = a.strings("fields");
        if !fields.is_empty() {
            q.push(("fields".to_string(), fields.join(",")));
        }
        if a.flag("brief") {
            q.push(("brief".to_string(), "1".to_string()));
        }
        let v = ctx.nb.get_with_fallback(&obj_path(&t, id), fb(&t, Some(id)).as_deref(), &q).await?;
        Ok(shape(&ctx, v, t.key == "users.token"))
    })
}

fn get_changelogs(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let filters = a.obj("filters");
        validate_filters(&filters)?;
        // Défaut 10 : une entrée porte l'objet avant/après en entier, la page par défaut de NetBox (50) est énorme.
        let q = list_query(&filters, &a.strings("fields"), false, limit_of(&a, 10), a.int("offset").unwrap_or(0), &a.string_or_list("ordering"));
        // `extras/object-changes` : NetBox < 4.1 (le serveur Python n'avait pas ce repli).
        let v = ctx.nb.get_with_fallback("core/object-changes", Some("extras/object-changes"), &q).await?;
        Ok(shape(&ctx, v, false))
    })
}

fn search_objects(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let wanted = {
            let given = a.strings("object_types");
            if given.is_empty() { DEFAULT_SEARCH_TYPES.iter().map(|s| s.to_string()).collect() } else { given }
        };
        let types: Vec<TypeInfo> = wanted.iter().map(|k| resolve(&ctx, k)).collect::<Result<_>>()?;
        let mut q = vec![("q".to_string(), a.text("query").to_string()), ("limit".to_string(), limit_of(&a, DEFAULT_LIMIT).to_string())];
        let fields = a.strings("fields");
        if !fields.is_empty() {
            q.push(("fields".to_string(), fields.join(",")));
        }
        let runs = types.iter().map(|t| {
            let (ctx, q, fields) = (ctx.clone(), q.clone(), fields.clone());
            let t = t.clone();
            async move {
                let mut r = ctx.nb.get_with_fallback(&t.endpoint, t.fallback.as_deref(), &q).await;
                // Un champ que ce type n'a pas peut faire échouer NetBox (500) : on relit sans `fields` et on garde les clés demandées ici.
                if !fields.is_empty() && r.as_ref().err().is_some_and(|e| !e.is_any_auth()) {
                    let plain: Vec<_> = q.iter().filter(|(k, _)| k != "fields").cloned().collect();
                    r = ctx.nb.get_with_fallback(&t.endpoint, t.fallback.as_deref(), &plain).await;
                }
                (t.key.clone(), r)
            }
        });
        let outcomes = join_all(runs).await;
        // Si NetBox refuse le jeton pour tous les types, ce n'est pas « aucun résultat » (le serveur Python répondait vide).
        if let Some((_, Err(e))) = outcomes.first() {
            if e.is_any_auth() && outcomes.iter().all(|(_, r)| r.as_ref().err().is_some_and(|e| e.is_any_auth())) {
                return Err(e.clone());
            }
        }
        let mut results = Map::new();
        let mut errors = Map::new();
        for (key, r) in outcomes {
            match r {
                Ok(v) => {
                    let rows = v.get("results").cloned().unwrap_or_else(|| json!([]));
                    results.insert(key, shape(&ctx, pick(rows, &fields), false));
                }
                Err(e) => {
                    results.insert(key.clone(), json!([]));
                    errors.insert(key, json!(e.message));
                }
            }
        }
        let mut out = Map::from_iter([("results".to_string(), Value::Object(results))]);
        if !errors.is_empty() {
            out.insert("errors".into(), Value::Object(errors));
        }
        Ok(Value::Object(out))
    })
}

fn list_object_types(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let needle = a.text("filter").to_ascii_lowercase();
        let rows: Vec<Value> = ctx
            .types
            .all()
            .into_iter()
            .filter(|t| needle.is_empty() || t.key.contains(&needle) || t.endpoint.contains(&needle) || t.name.to_ascii_lowercase().contains(&needle))
            .map(|t| json!({ "type": t.key, "name": t.name, "endpoint": t.endpoint, "plugin": t.plugin }))
            .collect();
        Ok(json!({ "count": rows.len(), "types": rows }))
    })
}

fn describe_type(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let schema = ctx.runtime.schema(&ctx.nb).await.map_err(|e| e.hint("The OpenAPI schema (/api/schema/) could not be read; field names can still be seen on an existing object with netbox_get_objects."))?;
        let mut d = crate::openapi::describe(schema, &t.endpoint)
            .ok_or_else(|| NbError::new(ErrorType::NotFound, format!("{} ({}) is not in this NetBox's OpenAPI schema.", t.key, t.endpoint)))?;
        d["type"] = json!(t.key);
        Ok(d)
    })
}

fn get_status(_a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move { ctx.nb.get("status", &[]).await })
}

fn api_get(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let path = api_path(a.text("path"))?;
        let v = ctx.nb.get(&path, &pairs(&a, "query")).await?;
        Ok(shape(&ctx, v, path.starts_with("users/tokens")))
    })
}

fn graphql(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let q = a.text("query");
        if regex::Regex::new(r"(?m)(^|[\s}])mutation\b").unwrap().is_match(q) {
            return Err(NbError::new(ErrorType::InvalidInput, "Mutations are not allowed through netbox_graphql.").hint("Use netbox_create_object / netbox_update_object / netbox_delete_object."));
        }
        let vars = a.0.get("variables").filter(|v| v.is_object());
        let v = ctx.nb.graphql(q, vars).await?;
        Ok(shape(&ctx, v, false))
    })
}

/// Parcours d'un câble/lien depuis une interface, un port console, etc. (`/trace/`).
fn trace(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let v = ctx.nb.get(&format!("{}/trace", obj_path(&t, object_id(&a)?)), &[]).await?;
        Ok(shape(&ctx, v, false))
    })
}

fn rack_elevation(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let id = a.int("rack_id").filter(|i| *i >= 1).ok_or_else(|| NbError::new(ErrorType::InvalidInput, "rack_id must be a positive integer."))?;
        let mut q = vec![("face".to_string(), a.str("face").unwrap_or("front").to_string())];
        q.push(("limit".into(), limit_of(&a, 100).to_string()));
        ctx.nb.get(&format!("dcim/racks/{id}/elevation"), &q).await
    })
}

/// Génère la configuration d'un équipement à partir de ses modèles. POST, mais ne modifie rien dans NetBox.
fn render_config(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.str("object_type").unwrap_or("dcim.device"))?;
        if !matches!(t.key.as_str(), "dcim.device" | "virtualization.virtualmachine") {
            return Err(NbError::new(ErrorType::InvalidInput, "render-config exists for dcim.device and virtualization.virtualmachine."));
        }
        let path = format!("{}/render-config", obj_path(&t, object_id(&a)?));
        let v = ctx.nb.request(Method::POST, &path, &[], Some(&json!({}))).await?;
        Ok(shape(&ctx, v, false))
    })
}

fn available_path(ctx: &Ctx, a: &Args) -> Result<String> {
    let t = resolve(ctx, a.text("parent_type"))?;
    let id = a.int("parent_id").filter(|i| *i >= 1).ok_or_else(|| NbError::new(ErrorType::InvalidInput, "parent_id must be a positive integer."))?;
    Ok(format!("{}/available-{}", obj_path(&t, id), a.text("what")))
}

fn get_available(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let path = available_path(&ctx, &a)?;
        let q = vec![("limit".to_string(), limit_of(&a, 10).to_string())];
        let v = ctx.nb.get(&path, &q).await?;
        Ok(match v {
            Value::Array(items) => json!({ "count": items.len(), "available": items }),
            other => other,
        })
    })
}

// ---------- écriture ----------

const MISSING_IN_DRY_RUN: &str = "Some of these objects do not exist. A real call would fail with 404 and change nothing; the request shown in `would_send` is what would be sent if they existed.";

fn create_object(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let data = non_empty_object(&a, "data")?;
        let v = ctx.write(Method::POST, &t.endpoint, &[], Some(&Value::Object(data))).await?;
        Ok(json!({ "created": pick(shape(&ctx, v, false), &a.strings("fields")) }))
    })
}

fn bulk_create(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let items = items_of(&a, &ctx, false)?;
        // NetBox crée en lot par un POST d'une liste sur le point de liste : il n'y a pas de chemin `/bulk/`
        // (le client Python en appelait un, qui répond 404).
        let v = ctx.write(Method::POST, &t.endpoint, &[], Some(&Value::Array(items.clone()))).await?;
        Ok(json!({ "count": items.len(), "created": pick(shape(&ctx, v, false), &a.strings("fields")) }))
    })
}

fn update_object(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let id = object_id(&a)?;
        let data = non_empty_object(&a, "data")?;
        // Valeurs actuelles des champs touchés : sert de « avant » pour annuler, et échoue tôt si l'objet n'existe pas.
        let mut fields = vec!["id".to_string(), "display".to_string()];
        fields.extend(data.keys().cloned());
        // En simulation, un objet absent n'arrête pas l'aperçu : la requête simulée est quand même montrée, avec l'avertissement.
        let previous = match ctx.nb.get_with_fallback(&obj_path(&t, id), fb(&t, Some(id)).as_deref(), &[("fields".into(), fields.join(","))]).await {
            Ok(current) => Some(shape(&ctx, current, false)),
            Err(e) if ctx.dry_run && e.is(ErrorType::NotFound) => None,
            Err(e) => return Err(e),
        };
        let v = ctx.write(Method::PATCH, &obj_path(&t, id), &[], Some(&Value::Object(data))).await?;
        if ctx.dry_run {
            return Ok(json!({ "exists": previous.is_some(), "current": previous, "warning": previous.is_none().then_some(MISSING_IN_DRY_RUN) }));
        }
        Ok(json!({ "updated": pick(shape(&ctx, v, false), &a.strings("fields")), "previous": previous, "revert_with": "netbox_update_object with the values in `previous`" }))
    })
}

fn bulk_update(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let items = items_of(&a, &ctx, true)?;
        let ids: Vec<Value> = items.iter().filter_map(|i| i.get("id").cloned()).collect();
        let q = vec![("limit".to_string(), ids.len().to_string()), ("id".to_string(), ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")), ("brief".to_string(), "1".to_string())];
        // `id` répété plutôt que séparé par des virgules : NetBox attend `id=1&id=2`.
        let q: Vec<(String, String)> = q.into_iter().filter(|(k, _)| k != "id").chain(ids.iter().map(|i| ("id".to_string(), i.to_string()))).collect();
        let existing = ctx.nb.get(&t.endpoint, &q).await?;
        let found = existing.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
        let missing = found != ids.len();
        if missing && !ctx.dry_run {
            return Err(NbError::new(ErrorType::NotFound, format!("{} id(s) requested, {} found: nothing was changed.", ids.len(), found)).hint("Check the ids with netbox_get_objects."));
        }
        let v = ctx.write(Method::PATCH, &t.endpoint, &[], Some(&Value::Array(items.clone()))).await?;
        if ctx.dry_run {
            return Ok(json!({ "count": items.len(), "found": found, "warning": missing.then_some(MISSING_IN_DRY_RUN) }));
        }
        Ok(json!({ "count": items.len(), "updated": pick(shape(&ctx, v, false), &a.strings("fields")) }))
    })
}

fn delete_object(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let id = object_id(&a)?;
        // Instantané avant suppression : permet de recréer l'objet, et échoue tôt s'il n'existe pas.
        let snapshot = match ctx.nb.get_with_fallback(&obj_path(&t, id), fb(&t, Some(id)).as_deref(), &[]).await {
            Ok(s) => Some(shape(&ctx, s, false)),
            Err(e) if ctx.dry_run && e.is(ErrorType::NotFound) => None,
            Err(e) => return Err(e),
        };
        ctx.write(Method::DELETE, &obj_path(&t, id), &[], None).await?;
        if ctx.dry_run {
            return Ok(json!({ "exists": snapshot.is_some(), "would_delete": snapshot, "warning": snapshot.is_none().then_some(MISSING_IN_DRY_RUN) }));
        }
        Ok(json!({ "deleted": snapshot, "note": "NetBox also removes objects that depend on this one (cascade). To undo, recreate it from `deleted` with netbox_create_object; the id will differ." }))
    })
}

fn bulk_delete(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let t = resolve(&ctx, a.text("object_type"))?;
        let ids: Vec<i64> = a.list("ids").iter().filter_map(Value::as_i64).filter(|i| *i >= 1).collect();
        if ids.is_empty() || ids.len() != a.list("ids").len() || ids.len() > ctx.cfg.max_bulk {
            return Err(NbError::new(ErrorType::InvalidInput, format!("`ids` must hold 1 to {} positive integers.", ctx.cfg.max_bulk)));
        }
        let q: Vec<(String, String)> = std::iter::once(("limit".to_string(), ids.len().to_string())).chain(ids.iter().map(|i| ("id".to_string(), i.to_string()))).collect();
        let existing = ctx.nb.get(&t.endpoint, &q).await?;
        let found = existing.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
        let missing = found != ids.len();
        if missing && !ctx.dry_run {
            return Err(NbError::new(ErrorType::NotFound, format!("{} id(s) requested, {} found: nothing was deleted.", ids.len(), found)));
        }
        let body = Value::Array(ids.iter().map(|i| json!({ "id": i })).collect());
        ctx.write(Method::DELETE, &t.endpoint, &[], Some(&body)).await?;
        let key = if ctx.dry_run { "would_delete" } else { "deleted" };
        Ok(json!({ key: shape(&ctx, existing.get("results").cloned().unwrap_or_default(), false), "count": ids.len(), "found": found, "warning": (ctx.dry_run && missing).then_some(MISSING_IN_DRY_RUN) }))
    })
}

fn allocate(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let path = available_path(&ctx, &a)?;
        let mut data = a.obj("data");
        if let Some(n) = a.int("prefix_length") {
            data.insert("prefix_length".into(), json!(n));
        }
        if a.text("what") == "prefixes" && !data.contains_key("prefix_length") {
            return Err(NbError::new(ErrorType::InvalidInput, "Allocating a prefix needs `prefix_length`."));
        }
        let count = a.int("count").unwrap_or(1).clamp(1, ctx.cfg.max_bulk as i64) as usize;
        // Un objet pour un, une liste pour plusieurs (NetBox les réserve ensemble ou pas du tout).
        let body = if count == 1 { Value::Object(data) } else { Value::Array(vec![Value::Object(data); count]) };
        let v = ctx.write(Method::POST, &path, &[], Some(&body)).await?;
        Ok(json!({ "allocated": pick(shape(&ctx, v, false), &a.strings("fields")) }))
    })
}

fn run_script(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let id = a.int("script_id").filter(|i| *i >= 1).ok_or_else(|| NbError::new(ErrorType::InvalidInput, "script_id must be a positive integer."))?;
        let mut body = json!({ "data": Value::Object(a.obj("data")), "commit": a.flag("commit") });
        if let Some(s) = a.str("schedule_at") {
            body["schedule_at"] = json!(s);
        }
        if let Some(i) = a.int("interval") {
            body["interval"] = json!(i);
        }
        let path = format!("extras/scripts/{id}");
        // En simulation, on vérifie que le script existe : un vrai appel répondrait 404.
        let script = if ctx.dry_run {
            match ctx.nb.get(&path, &[]).await {
                Ok(s) => Some(pick(shape(&ctx, s, false), &["id".into(), "name".into(), "display".into()])),
                Err(e) if e.is(ErrorType::NotFound) => None,
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        let v = ctx.write(Method::POST, &path, &[], Some(&body)).await?;
        if ctx.dry_run {
            return Ok(json!({ "exists": script.is_some(), "script": script, "warning": script.is_none().then_some("No script with this id: a real call would fail with 404 and run nothing.") }));
        }
        Ok(shape(&ctx, v, false))
    })
}

fn api_write(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let path = api_path(a.text("path"))?;
        let method = match a.text("method") {
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            m => return Err(NbError::new(ErrorType::InvalidInput, format!("Unsupported method {m}."))),
        };
        let body = a.0.get("body").filter(|b| b.is_object() || b.is_array());
        let v = ctx.write(method, &path, &pairs(&a, "query"), body).await?;
        Ok(shape(&ctx, v, false))
    })
}

fn api_delete(a: Args, ctx: Arc<Ctx>) -> BoxFut<Result<Value>> {
    boxed(async move {
        let path = api_path(a.text("path"))?;
        let body = a.0.get("body").filter(|b| b.is_array());
        let v = ctx.write(Method::DELETE, &path, &pairs(&a, "query"), body).await?;
        Ok(json!({ "deleted": path, "response": v }))
    })
}

// ---------- confirmations ----------

fn confirm_delete(a: &Args) -> String {
    format!("Delete {} #{} in NetBox? Dependent objects are removed too.", a.text("object_type"), a.int("object_id").unwrap_or(0))
}
fn confirm_bulk_delete(a: &Args) -> String {
    format!("Delete {} {} object(s) in NetBox: ids {}?", a.list("ids").len(), a.text("object_type"), a.0.get("ids").map(Value::to_string).unwrap_or_default())
}
fn confirm_api_delete(a: &Args) -> String {
    format!("Send DELETE /api/{} to NetBox?", a.text("path").trim_matches('/'))
}
fn confirm_script(a: &Args) -> String {
    format!("Run NetBox script #{} with commit={}?", a.int("script_id").unwrap_or(0), a.flag("commit"))
}
fn confirm_update(a: &Args) -> String {
    format!("Update {} #{} in NetBox with {}?", a.text("object_type"), a.int("object_id").unwrap_or(0), a.0.get("data").map(Value::to_string).unwrap_or_default())
}
fn confirm_create(a: &Args) -> String {
    format!("Create a {} in NetBox with {}?", a.text("object_type"), a.0.get("data").map(Value::to_string).unwrap_or_default())
}
fn confirm_bulk(a: &Args) -> String {
    format!("{} {} item(s) of type {} in NetBox?", "Write", a.list("items").len(), a.text("object_type"))
}
fn confirm_allocate(a: &Args) -> String {
    format!("Allocate {} x {} from {} #{} in NetBox?", a.int("count").unwrap_or(1), a.text("what"), a.text("parent_type"), a.int("parent_id").unwrap_or(0))
}
fn confirm_api_write(a: &Args) -> String {
    format!("Send {} /api/{} to NetBox with {}?", a.text("method"), a.text("path").trim_matches('/'), a.0.get("body").map(Value::to_string).unwrap_or_default())
}

// ---------- catalogue ----------

pub fn all_tools(cfg: &Config, type_keys: &[String]) -> Vec<ToolDef> {
    let order = || any("ordering").opt().desc("Sort fields; prefix '-' to reverse. A string or a list, e.g. \"name\" or [\"facility\",\"-name\"].");
    let parent_types = "ipam.prefix (ips, prefixes), ipam.iprange (ips), ipam.aggregate (prefixes), ipam.vlangroup (vlans), ipam.asnrange (asns)";
    let pfields = || strings("fields").opt().desc("Top-level fields to keep in the answer.");
    let tools = vec![
        read(
            "netbox_get_objects",
            "Get NetBox objects",
            format!(
                "Get objects from NetBox by type and filters. Returns NetBox's paginated answer: count (total matches), next, previous, results. ALWAYS read `count` and `next`: results are paginated.\n\
                 Use `fields` to cut tokens. Filters: direct fields ({{\"site_id\":1,\"name\":\"r1\",\"status\":\"active\"}}) or lookups ({{\"name__ic\":\"sw\",\"vid__gte\":100}}; suffixes n, ic, nic, isw, nisw, iew, niew, ie, nie, empty, regex, iregex, lt, lte, gt, gte, supported per field). Several values: pass a list ({{\"id\":[1,2,3]}}). No \"__in\", no multi-hop: do two calls (find the site, then {{\"site_id\": id}}).\n\
                 Valid object_type values (netbox_list_object_types gives names and endpoints; plugin types are included when discovered):\n{}",
                type_keys.join(", ")
            ),
            vec![f_type(), f_filters(), f_fields(), f_brief(), f_limit(cfg), f_offset(), order()],
            get_objects,
        ),
        read(
            "netbox_get_object_by_id",
            "Get one NetBox object",
            "Get one NetBox object by numeric id. Use `fields` to cut tokens.",
            vec![f_type(), integer("object_id").min(1).desc("Numeric id."), f_fields(), f_brief()],
            get_object_by_id,
        ),
        read(
            "netbox_get_changelogs",
            "NetBox change log",
            "Get object change records (who changed what, before/after). Filters: user_id, user, changed_object_type_id (numeric ContentType id, not a string), changed_object_id, object_repr, action (create | update | delete), time_before / time_after (ISO 8601), request_id, q. Example: {\"changed_object_id\": 123, \"action\": \"update\"}. Entries carry full prechange_data/postchange_data: keep `limit` small (default 10) and use `fields` (e.g. [\"id\",\"time\",\"user_name\",\"action\",\"object_repr\"]).",
            vec![f_filters(), f_fields(), f_limit(cfg), f_offset(), order()],
            get_changelogs,
        ),
        read(
            "netbox_search_objects",
            "Search NetBox",
            format!(
                "Global search (`q`) across several object types at once: names, descriptions, IPs, serials, asset tags. Default types: {}. Returns {{results: {{type: [objects]}}, errors?}}; a type that failed appears in `errors` instead of silently looking empty.",
                DEFAULT_SEARCH_TYPES.join(", ")
            ),
            vec![string("query").desc("Search term, e.g. \"switch01\", \"192.168.1.1\", \"NYC-DC1\"."), strings("object_types").opt().desc("Limit the search to these types."), pfields(), f_limit(cfg)],
            search_objects,
        ),
        read(
            "netbox_list_object_types",
            "List object types",
            "List the object types this server can reach (core, new in this NetBox version, and plugin models) with their API endpoint. `filter` matches the type, name or endpoint.",
            vec![string("filter").opt().desc("Substring, e.g. \"dns\" or \"vlan\".")],
            list_object_types,
        ),
        read(
            "netbox_describe_type",
            "Describe an object type",
            "Fields (type, required, choices, nullable), accepted filters and extra actions (trace, available-ips, plugin actions...) of an object type, read from this NetBox's own OpenAPI schema. Call it BEFORE creating or updating an object, and to learn what a plugin offers.",
            vec![f_type()],
            describe_type,
        ),
        read("netbox_get_status", "NetBox status", "NetBox version, Django/Python versions, installed plugins and their versions, workers.", vec![], get_status),
        read(
            "netbox_api_get",
            "Raw NetBox GET",
            "GET any NetBox API path, for what the typed tools do not cover: plugin endpoints and custom actions, extras/scripts, core/jobs, dcim/devices/{id}/napalm, connected-device, etc. `path` is relative to /api (e.g. \"plugins/netbox-dns/zones\", \"dcim/connected-device\"); filters go in `query`. netbox_describe_type lists a type's actions.",
            vec![string("path").desc("API path relative to /api."), unknown_record("query").opt().desc("Query parameters.")],
            api_get,
        ),
        read(
            "netbox_graphql",
            "NetBox GraphQL query",
            "Run a read-only GraphQL query: one call can fetch an object with its relations and only the fields you name, often far cheaper than several REST calls. Mutations are refused.",
            vec![string("query").desc("GraphQL query."), unknown_record("variables").opt().desc("Query variables.")],
            graphql,
        ),
        read(
            "netbox_trace",
            "Trace a cable path",
            "Cable path from an interface, console port, console server port, power port, power outlet or front/rear port (/trace/): returns the hops end to end.",
            vec![f_type().desc("e.g. dcim.interface, dcim.consoleport, dcim.powerport, dcim.frontport"), integer("object_id").min(1).desc("Numeric id.")],
            trace,
        ),
        read(
            "netbox_rack_elevation",
            "Rack elevation",
            "Rack units of a rack with what occupies them (JSON).",
            vec![integer("rack_id").min(1).desc("Rack id."), enumeration("face", &["front", "rear"]).opt().desc("Default front."), integer("limit").opt().min(1).max(500).desc("Max units (default 100).")],
            rack_elevation,
        ),
        read(
            "netbox_render_config",
            "Render device config",
            "Render a device's (or virtual machine's) configuration from its config template. Changes nothing in NetBox.",
            vec![string("object_type").opt().desc("dcim.device (default) or virtualization.virtualmachine."), integer("object_id").min(1).desc("Device or VM id.")],
            render_config,
        ),
        read(
            "netbox_get_available",
            "Available IPs, prefixes, VLANs, ASNs",
            format!("List what is still free inside a parent: IPs in a prefix or IP range, child prefixes in a prefix, VLAN ids in a VLAN group, ASNs in an ASN range. parent_type: {parent_types}."),
            vec![string("parent_type").desc("Parent object type."), integer("parent_id").min(1).desc("Parent id."), enumeration("what", &["ips", "prefixes", "vlans", "asns"]), integer("limit").opt().min(1).max(cfg.max_limit).desc("Max results (default 10).")],
            get_available,
        ),
        write(
            "netbox_create_object",
            "Create a NetBox object",
            "Create one object. `data` holds the fields (call netbox_describe_type first: required fields, choices). Foreign keys take the numeric id of the target. Returns the created object (limit with `fields`).",
            vec![f_type(), unknown_record("data").desc("Fields of the new object, e.g. {\"name\":\"r1\",\"site\":3,\"role\":2,\"device_type\":7}."), pfields()],
            Access::Write,
            false,
            create_object,
            Some(precheck_type as Precheck),
            Some(confirm_create as Confirmation),
        ),
        write(
            "netbox_bulk_create",
            "Create several NetBox objects",
            "Create several objects of one type in one request (all or none). `items` is a list of field objects.",
            vec![f_type(), array_objects("items", "Objects to create."), pfields()],
            Access::Write,
            false,
            bulk_create,
            Some(precheck_type as Precheck),
            Some(confirm_bulk as Confirmation),
        ),
        write(
            "netbox_update_object",
            "Update a NetBox object",
            "Partial update (PATCH) of one object: only the fields in `data` change. Returns the new object and `previous`, the old values of the changed fields, to revert. To clear a field send null.",
            vec![f_type(), integer("object_id").min(1).desc("Numeric id."), unknown_record("data").desc("Fields to change, e.g. {\"status\":\"decommissioning\"}."), pfields()],
            Access::Write,
            true,
            update_object,
            Some(precheck_type as Precheck),
            Some(confirm_update as Confirmation),
        ),
        write(
            "netbox_bulk_update",
            "Update several NetBox objects",
            "Partial update of several objects of one type in one request (all or none). Each item needs its `id` plus the fields to change. Checks first that every id exists.",
            vec![f_type(), array_objects("items", "Objects to update, each with `id`."), pfields()],
            Access::Write,
            true,
            bulk_update,
            Some(precheck_type as Precheck),
            Some(confirm_bulk as Confirmation),
        ),
        write(
            "netbox_delete_object",
            "Delete a NetBox object",
            "Delete one object. NetBox also deletes the objects that depend on it (deleting a device removes its interfaces...). Returns the full object as it was, to recreate it if needed.",
            vec![f_type(), integer("object_id").min(1).desc("Numeric id.")],
            Access::Destructive,
            true,
            delete_object,
            Some(precheck_type as Precheck),
            Some(confirm_delete as Confirmation),
        ),
        write(
            "netbox_bulk_delete",
            "Delete several NetBox objects",
            "Delete several objects of one type in one request (all or none). Checks first that every id exists. Cascades like netbox_delete_object.",
            vec![f_type(), array_ints("ids", "Ids to delete.")],
            Access::Destructive,
            true,
            bulk_delete,
            Some(precheck_type as Precheck),
            Some(confirm_bulk_delete as Confirmation),
        ),
        write(
            "netbox_allocate",
            "Allocate next free IP, prefix, VLAN or ASN",
            format!("Create the next free IP address(es), child prefix(es), VLAN(s) or ASN(s) inside a parent, race-free (NetBox picks). parent_type: {parent_types}. `what=prefixes` needs `prefix_length`. `data` holds the fields of the new object (status, description, tenant, vrf... ; VLANs need name; ASNs need nothing more). `count` > 1 reserves several at once."),
            vec![string("parent_type").desc("Parent object type."), integer("parent_id").min(1).desc("Parent id."), enumeration("what", &["ips", "prefixes", "vlans", "asns"]), integer("prefix_length").opt().min(1).max(128).desc("For prefixes."), unknown_record("data").opt().desc("Fields of the new object(s)."), integer("count").opt().min(1).desc("How many (default 1)."), pfields()],
            Access::Write,
            false,
            allocate,
            Some(precheck_type as Precheck),
            Some(confirm_allocate as Confirmation),
        ),
        write(
            "netbox_run_script",
            "Run a NetBox custom script",
            "Run a custom script (extras/scripts). commit=false (default) runs it without saving database changes; commit=true applies them. A script can change anything: read what it does first (netbox_api_get extras/scripts/{id}).",
            vec![integer("script_id").min(1).desc("Script id."), unknown_record("data").opt().desc("Script variables."), boolean("commit").opt().desc("Save the script's changes (default false)."), string("schedule_at").opt().desc("ISO 8601 time to run later."), integer("interval").opt().min(1).desc("Repeat every N minutes.")],
            Access::Destructive,
            false,
            run_script,
            None,
            Some(confirm_script as Confirmation),
        ),
        write(
            "netbox_api_write",
            "Raw NetBox write",
            "POST, PUT or PATCH any NetBox API path, for what the typed tools do not cover: plugin objects and custom actions, jobs, journal entries, etc. Subject to the same write rules as the typed tools. Prefer netbox_create_object / netbox_update_object when they fit.",
            vec![enumeration("method", &["POST", "PUT", "PATCH"]), string("path").desc("API path relative to /api."), unknown_record("query").opt().desc("Query parameters."), any("body").opt().desc("JSON body: object or list.")],
            Access::Write,
            false,
            api_write,
            Some(precheck_path as Precheck),
            Some(confirm_api_write as Confirmation),
        ),
        write(
            "netbox_api_delete",
            "Raw NetBox DELETE",
            "DELETE any NetBox API path (an object, or a list endpoint with a body of [{\"id\": n}]). Same rules as the typed tools. Cascades.",
            vec![string("path").desc("API path relative to /api, e.g. plugins/netbox-dns/zones/4."), unknown_record("query").opt().desc("Query parameters."), any("body").opt().desc("List of {\"id\": n} for bulk deletion.")],
            Access::Destructive,
            true,
            api_delete,
            Some(precheck_path as Precheck),
            Some(confirm_api_delete as Confirmation),
        ),
    ];
    tools.into_iter().filter(|t| !cfg.disabled_tools.contains(t.name)).collect()
}

fn array_objects(name: &'static str, desc: &str) -> Field {
    crate::schema::array_of(name, crate::schema::Ty::Record(vec![crate::schema::Ty::Unknown])).desc(desc)
}

fn array_ints(name: &'static str, desc: &str) -> Field {
    crate::schema::array_of(name, crate::schema::Ty::Int { min: Some(1), max: None }).desc(desc)
}

trait AuthKind {
    fn is_any_auth(&self) -> bool;
}
impl AuthKind for NbError {
    fn is_any_auth(&self) -> bool {
        self.is(ErrorType::AuthFailed) || self.is(ErrorType::Forbidden)
    }
}
