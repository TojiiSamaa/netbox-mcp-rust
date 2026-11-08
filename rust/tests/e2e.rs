//! Bout en bout contre un faux NetBox : le serveur MCP est piloté en JSON-RPC (Session), le faux NetBox
//! enregistre chaque requête reçue. Prouve ce que le serveur envoie réellement, pas ce qu'il prétend envoyer.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use nbmcp::config::{load_config, Env};
use nbmcp::runtime::Runtime;
use nbmcp::server::{Registry, Session};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<(String, String, Value)>>>;

async fn fake(State(log): State<Log>, method: Method, uri: Uri, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_string();
    let full = format!("{path}?{}", uri.query().unwrap_or(""));
    log.lock().unwrap().push((method.to_string(), full.clone(), body.clone()));
    let json = |v: Value| (StatusCode::OK, [("content-type", "application/json")], v.to_string()).into_response();
    match (method.as_str(), path.as_str()) {
        ("GET", "/api/dcim/sites/") => json(json!({ "count": 1, "next": null, "previous": null, "results": [{ "id": 1, "name": "s1", "secret": "x", "preshared_key": "p" }] })),
        ("GET", "/api/dcim/sites/1/") => json(json!({ "id": 1, "name": "s1", "display": "s1", "status": "active" })),
        ("GET", "/api/dcim/sites/404/") => (StatusCode::NOT_FOUND, [("content-type", "application/json")], json!({ "detail": "Not found." }).to_string()).into_response(),
        ("PATCH", "/api/dcim/sites/1/") => json(json!({ "id": 1, "name": "s1", "status": "retired" })),
        ("DELETE", "/api/dcim/sites/1/") => StatusCode::NO_CONTENT.into_response(),
        ("POST", "/api/dcim/sites/") => (StatusCode::CREATED, [("content-type", "application/json")], json!({ "id": 9, "name": "new" }).to_string()).into_response(),
        ("GET", "/api/core/object-changes/") => (StatusCode::NOT_FOUND, "<html>nope</html>").into_response(),
        ("GET", "/api/extras/object-changes/") => json(json!({ "count": 0, "results": [] })),
        ("GET", "/api/status/") => json(json!({ "netbox-version": "4.2.0", "plugins": { "netbox_dns": "1.0" } })),
        ("GET", "/api/core/object-types/") => json(json!({ "count": 1, "next": null, "results": [{ "app_label": "netbox_dns", "model": "zone", "rest_api_endpoint": "/api/plugins/netbox-dns/zones/", "is_plugin_model": true }] })),
        ("GET", "/api/plugins/netbox-dns/zones/") => json(json!({ "count": 0, "results": [] })),
        ("POST", "/api/ipam/prefixes/5/available-ips/") => (StatusCode::CREATED, [("content-type", "application/json")], json!({ "id": 3, "address": "10.0.0.1/24" }).to_string()).into_response(),
        _ => (StatusCode::NOT_FOUND, "<html>nope</html>").into_response(),
    }
}

async fn start(extra: &[(&str, &str)]) -> (Arc<Session>, Log) {
    let (s, log, _) = start_with(extra, false).await;
    (s, log)
}

/// `channel` : la session a un canal sortant (stdio), donc peut envoyer une requête d'elicitation.
async fn start_with(extra: &[(&str, &str)], channel: bool) -> (Arc<Session>, Log, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let log: Log = Arc::default();
    let app = Router::new().fallback(fake).with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut env: Env = [("NETBOX_URL", url.as_str()), ("NETBOX_TOKEN", "tok")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let rt = Runtime::new(load_config(&env).unwrap()).await.unwrap();
    (Session::new(Registry::new(rt), channel.then_some(tx), None), log, rx)
}

async fn call(s: &Arc<Session>, name: &str, args: Value) -> (bool, Value) {
    let r = s.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } })).await.unwrap();
    let res = &r["result"];
    let text = res["content"][0]["text"].as_str().unwrap_or("null");
    (res["isError"] == true, serde_json::from_str(text).unwrap_or(Value::Null))
}

async fn names(s: &Arc<Session>) -> Vec<String> {
    let r = s.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await.unwrap();
    r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

fn sent(log: &Log, method: &str) -> Vec<(String, Value)> {
    log.lock().unwrap().iter().filter(|(m, _, _)| m == method).map(|(_, p, b)| (p.clone(), b.clone())).collect()
}

#[tokio::test]
async fn read_only_by_default() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false")]).await;
    let n = names(&s).await;
    assert!(n.contains(&"netbox_get_objects".into()));
    assert!(!n.iter().any(|x| x.contains("create") || x.contains("delete") || x.contains("update")), "{n:?}");
    // même appelé de force, l'outil n'existe pas
    let r = s.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "netbox_create_object", "arguments": {} } })).await.unwrap();
    assert!(r.get("error").is_some());
    assert!(sent(&log, "POST").is_empty());
}

#[tokio::test]
async fn reads_redact_secrets_and_use_the_token_scheme() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false")]).await;
    let (err, v) = call(&s, "netbox_get_objects", json!({ "object_type": "dcim.site", "filters": { "id": [1, 2] }, "fields": ["id", "name"] })).await;
    assert!(!err, "{v}");
    assert_eq!(v["results"][0]["secret"], "[redacted]");
    assert_eq!(v["results"][0]["preshared_key"], "[redacted]");
    let (_, path, _) = log.lock().unwrap()[0].clone();
    assert!(path.contains("id=1") && path.contains("id=2") && path.contains("fields=id%2Cname") && path.contains("limit=5"), "{path}");
}

#[tokio::test]
async fn invalid_filters_never_reach_netbox() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false")]).await;
    let (err, v) = call(&s, "netbox_get_objects", json!({ "object_type": "dcim.site", "filters": { "name__in": "a,b" } })).await;
    assert!(err, "{v}");
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn write_flow_update_previous_delete_snapshot() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true")]).await;
    let n = names(&s).await;
    assert!(n.contains(&"netbox_update_object".into()) && !n.contains(&"netbox_delete_object".into()));

    let (err, v) = call(&s, "netbox_update_object", json!({ "object_type": "dcim.site", "object_id": 1, "data": { "status": "retired" } })).await;
    assert!(!err, "{v}");
    assert_eq!(v["updated"]["status"], "retired");
    assert_eq!(v["previous"]["status"], "active");
    assert_eq!(sent(&log, "PATCH")[0].1, json!({ "status": "retired" }));

    let (err, v) = call(&s, "netbox_create_object", json!({ "object_type": "dcim.site", "data": { "name": "new" } })).await;
    assert!(!err && v["created"]["id"] == 9, "{v}");
}

#[tokio::test]
async fn delete_needs_its_own_switch_and_returns_a_snapshot() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true"), ("NETBOX_ALLOW_DELETE", "true"), ("NETBOX_ELICIT", "off")]).await;
    let (err, v) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 1 })).await;
    assert!(!err, "{v}");
    assert_eq!(v["deleted"]["name"], "s1");
    assert_eq!(sent(&log, "DELETE").len(), 1);
    // objet absent : échoue avant d'envoyer le DELETE
    let (err, _) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 404 })).await;
    assert!(err);
    assert_eq!(sent(&log, "DELETE").len(), 1);
}

#[tokio::test]
async fn destructive_calls_fail_closed_without_a_way_to_confirm() {
    // Défaut (NETBOX_ELICIT=destructive) et session sans canal ni elicitation : refus avant toute requête.
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true"), ("NETBOX_ALLOW_DELETE", "true")]).await;
    let (err, v) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 1 })).await;
    assert!(err && v.to_string().contains("confirmation_required"), "{v}");
    let (err, v) = call(&s, "netbox_bulk_delete", json!({ "object_type": "dcim.site", "ids": [1] })).await;
    assert!(err && v.to_string().contains("confirmation_required"), "{v}");
    assert!(sent(&log, "DELETE").is_empty());
    // Les écritures non destructives ne sont pas concernées par le défaut.
    let (err, v) = call(&s, "netbox_create_object", json!({ "object_type": "dcim.site", "data": { "name": "new" } })).await;
    assert!(!err, "{v}");
}

#[tokio::test]
async fn confirm_argument_is_an_opt_in_fallback() {
    let base = [("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true"), ("NETBOX_ALLOW_DELETE", "true")];
    // Sans l'option, `confirm: true` ne change rien : le défaut reste fermé.
    let (s, log) = start(&base).await;
    let (err, v) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 1, "confirm": true })).await;
    assert!(err && v.to_string().contains("confirmation_required"), "{v}");
    assert!(sent(&log, "DELETE").is_empty());
    // Avec NETBOX_CONFIRM_ARG : refus sans l'argument, succès avec.
    let mut on = base.to_vec();
    on.push(("NETBOX_CONFIRM_ARG", "true"));
    let (s, log) = start(&on).await;
    let (err, v) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 1 })).await;
    assert!(err && v.to_string().contains("confirm: true"), "{v}");
    assert!(sent(&log, "DELETE").is_empty());
    let (err, v) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 1, "confirm": true })).await;
    assert!(!err, "{v}");
    assert_eq!(sent(&log, "DELETE").len(), 1);
}

#[tokio::test]
async fn dry_run_sends_no_write() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_DRY_RUN", "true")]).await;
    let (err, v) = call(&s, "netbox_update_object", json!({ "object_type": "dcim.site", "object_id": 1, "data": { "status": "retired" } })).await;
    assert!(!err, "{v}");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["would_send"][0]["method"], "PATCH");
    assert!(sent(&log, "PATCH").is_empty());
    let (_, v) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 1 })).await;
    assert!(v["would_delete"].is_object());
    assert!(sent(&log, "DELETE").is_empty());
}

#[tokio::test]
async fn type_policy_blocks_before_any_request() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true"), ("NETBOX_WRITE_TYPES", "ipam.*")]).await;
    let (err, v) = call(&s, "netbox_create_object", json!({ "object_type": "dcim.site", "data": { "name": "x" } })).await;
    assert!(err && v.to_string().contains("type_not_allowed"), "{v}");
    assert!(log.lock().unwrap().is_empty());
    // le chemin brut ne contourne pas la politique
    let (err, _) = call(&s, "netbox_api_write", json!({ "method": "POST", "path": "dcim/sites", "body": { "name": "x" } })).await;
    assert!(err);
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bulk_create_posts_a_list_to_the_list_endpoint() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true")]).await;
    let (_, _) = call(&s, "netbox_bulk_create", json!({ "object_type": "dcim.site", "items": [{ "name": "a" }, { "name": "b" }] })).await;
    let posts = sent(&log, "POST");
    assert_eq!(posts[0].0, "/api/dcim/sites/?");
    assert_eq!(posts[0].1, json!([{ "name": "a" }, { "name": "b" }]));
}

#[tokio::test]
async fn allocate_posts_to_available_ips() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true")]).await;
    let (err, v) = call(&s, "netbox_allocate", json!({ "parent_type": "ipam.prefix", "parent_id": 5, "what": "ips", "data": { "status": "active" } })).await;
    assert!(!err, "{v}");
    assert_eq!(v["allocated"]["address"], "10.0.0.1/24");
    assert_eq!(sent(&log, "POST")[0].0, "/api/ipam/prefixes/5/available-ips/?");
}

#[tokio::test]
async fn changelog_falls_back_for_older_netbox() {
    let (s, _) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false")]).await;
    let (err, v) = call(&s, "netbox_get_changelogs", json!({})).await;
    assert!(!err, "{v}");
    assert_eq!(v["count"], 0);
}

#[tokio::test]
async fn plugin_types_are_discovered_and_usable() {
    let (s, _) = start(&[]).await;
    let (err, v) = call(&s, "netbox_get_objects", json!({ "object_type": "netbox_dns.zone" })).await;
    assert!(!err, "{v}");
    assert_eq!(v["count"], 0);
}

#[tokio::test]
async fn graphql_refuses_mutations() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false")]).await;
    let (err, _) = call(&s, "netbox_graphql", json!({ "query": "mutation { x }" })).await;
    assert!(err);
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn elicitation_accept_and_refuse_when_the_client_supports_it() {
    let env = [("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true"), ("NETBOX_ALLOW_DELETE", "true")];
    let (s, log, mut rx) = start_with(&env, true).await;
    s.handle(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": { "elicitation": {} }, "clientInfo": { "name": "t", "version": "1" } } })).await.unwrap();
    for (accept, deletes) in [(false, 0), (true, 1)] {
        let s2 = s.clone();
        let task = tokio::spawn(async move { call(&s2, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 1 })).await });
        let req = rx.recv().await.unwrap();
        assert_eq!(req["method"], "elicitation/create");
        let result = if accept { json!({ "action": "accept", "content": { "confirm": true } }) } else { json!({ "action": "decline" }) };
        s.deliver_response(json!({ "jsonrpc": "2.0", "id": req["id"], "result": result }));
        let (err, v) = task.await.unwrap();
        assert_eq!(err, !accept, "{v}");
        assert_eq!(sent(&log, "DELETE").len(), deletes);
    }
}

fn writes_received(log: &Log) -> usize {
    log.lock().unwrap().iter().filter(|(m, p, _)| m != "GET" && !p.contains("/graphql")).count()
}

#[tokio::test]
async fn dry_run_env_forces_the_simulation_even_when_the_call_says_dry_run_false() {
    // Exigence : avec NETBOX_DRY_RUN=true, aucun appel ne peut écrire, quoi que dise l'argument `dry_run`.
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_DRY_RUN", "true")]).await;
    let site = json!({ "object_type": "dcim.site" });
    let with = |extra: Value| {
        let mut v = site.clone();
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v.as_object_mut().unwrap().insert("dry_run".into(), json!(false));
        v
    };
    let calls = [
        ("netbox_create_object", with(json!({ "data": { "name": "x", "slug": "x" } }))),
        ("netbox_bulk_create", with(json!({ "items": [{ "name": "x", "slug": "x" }] }))),
        ("netbox_update_object", with(json!({ "object_id": 1, "data": { "status": "retired" } }))),
        ("netbox_bulk_update", with(json!({ "items": [{ "id": 1, "status": "retired" }] }))),
        ("netbox_delete_object", with(json!({ "object_id": 1 }))),
        ("netbox_bulk_delete", with(json!({ "ids": [1] }))),
        ("netbox_allocate", json!({ "parent_type": "ipam.prefix", "parent_id": 5, "what": "ips", "dry_run": false })),
        ("netbox_run_script", json!({ "script_id": 2, "commit": true, "dry_run": false })),
        ("netbox_api_write", json!({ "path": "dcim/sites", "method": "POST", "body": { "name": "x" }, "dry_run": false })),
        ("netbox_api_delete", json!({ "path": "dcim/sites/1", "dry_run": false })),
    ];
    for (tool, args) in calls {
        let (err, v) = call(&s, tool, args).await;
        assert!(!err, "{tool}: {v}");
        assert_eq!(v["dry_run"], true, "{tool}: {v}");
        assert!(!v["would_send"].as_array().unwrap().is_empty(), "{tool} must show the request it would send");
    }
    assert_eq!(writes_received(&log), 0, "NetBox received a write: {:?}", log.lock().unwrap());
}

#[tokio::test]
async fn dry_run_previews_updates_and_deletes_of_missing_objects() {
    let (s, log) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_DRY_RUN", "true")]).await;
    let (err, v) = call(&s, "netbox_update_object", json!({ "object_type": "dcim.site", "object_id": 404, "data": { "status": "retired" } })).await;
    assert!(!err, "{v}");
    assert_eq!((v["exists"].clone(), v["would_send"][0]["method"].clone(), v["would_send"][0]["path"].clone()), (json!(false), json!("PATCH"), json!("/api/dcim/sites/404/")));
    assert!(v["warning"].as_str().unwrap().contains("404"));
    let (err, v) = call(&s, "netbox_delete_object", json!({ "object_type": "dcim.site", "object_id": 404 })).await;
    assert!(!err && v["exists"] == false && v["would_send"][0]["method"] == "DELETE", "{v}");
    // le faux NetBox répond « 1 trouvé » pour une liste : 2 ids demandés = un manquant
    let (err, v) = call(&s, "netbox_bulk_update", json!({ "object_type": "dcim.site", "items": [{ "id": 1, "status": "x" }, { "id": 404, "status": "x" }] })).await;
    assert!(!err && v["found"] == 1 && v["warning"].is_string() && v["would_send"][0]["method"] == "PATCH", "{v}");
    let (err, v) = call(&s, "netbox_bulk_delete", json!({ "object_type": "dcim.site", "ids": [1, 404] })).await;
    assert!(!err && v["found"] == 1 && v["warning"].is_string() && v["would_send"][0]["method"] == "DELETE", "{v}");
    assert_eq!(writes_received(&log), 0);
}

#[tokio::test]
async fn reading_a_missing_object_is_an_answer_not_a_tool_error() {
    let (s, _) = start(&[("ENABLE_PLUGIN_DISCOVERY", "false")]).await;
    let (err, v) = call(&s, "netbox_get_object_by_id", json!({ "object_type": "dcim.site", "object_id": 404 })).await;
    assert!(!err && v["found"] == false && v["error"]["error_type"] == "not_found", "{v}");
    // une adresse inconnue (404 HTML) reste une vraie erreur
    let (err, _) = call(&s, "netbox_api_get", json!({ "path": "nope/nothing" })).await;
    assert!(err);
}

async fn tool_schema(s: &Arc<Session>, name: &str) -> Value {
    let r = s.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await.unwrap();
    r["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["inputSchema"].clone()
}

#[tokio::test]
async fn confirm_is_published_in_the_schema_when_the_fallback_is_on() {
    // Issue #5 : le schéma publié est fermé, `confirm` doit y figurer pour les outils qui demandent confirmation.
    let base = [("ENABLE_PLUGIN_DISCOVERY", "false"), ("NETBOX_ALLOW_WRITES", "true"), ("NETBOX_ALLOW_DELETE", "true")];
    let (s, _) = start(&[base.as_slice(), &[("NETBOX_CONFIRM_ARG", "true")]].concat()).await;
    for tool in ["netbox_delete_object", "netbox_bulk_delete", "netbox_run_script", "netbox_api_delete"] {
        assert_eq!(tool_schema(&s, tool).await["properties"]["confirm"]["type"], "boolean", "{tool}");
    }
    // pas de confirmation demandée pour une simple création (NETBOX_ELICIT=destructive) : pas de paramètre en trop
    assert!(tool_schema(&s, "netbox_create_object").await["properties"].get("confirm").is_none());
    // repli désactivé : `confirm` n'est pas annoncé
    let (s, _) = start(&base).await;
    assert!(tool_schema(&s, "netbox_delete_object").await["properties"].get("confirm").is_none());
}
