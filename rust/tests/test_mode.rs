//! Mode test (MCP_TEST_MODE / --test) : tous les outils répondent depuis le mock, rien de réel ne part.
//!
//! - preuve A dynamique (`proof_a_*`) : le transport réel est un `Tripwire` qui panique ; tous les outils sont appelés
//!   (avec chaque approbateur) sans qu'il soit touché. La preuve A statique est dans `egress_static.rs`.
//! - preuve B (`proof_b_*`) : le vrai binaire, lancé avec une URL NetBox d'apparence réelle qui pointe vers un écouteur
//!   sentinelle local ; tous les outils sont appelés, l'écouteur ne reçoit aucune connexion. Lancé sous isolement réseau
//!   OS (`docker --network none` ou `unshare -n`, voir le workflow CI), avec NBMCP_ISOLATED=1 pour le prouver aussi.
//! - validation admin : 4 scénarios (approuvé, refusé, expiré, client sans elicitation) pour chaque outil à validation.

use nbmcp::config::load_config;
use nbmcp::runtime::Runtime;
use nbmcp::server::{Registry, Session};
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TOOL_COUNT: usize = 24;

fn calls() -> Map<String, Value> {
    let text = include_str!("../test/fixtures/calls.json");
    let mut m: Map<String, Value> = serde_json::from_str(text).unwrap();
    m.remove("_doc");
    m
}

async fn session(approver: Option<&str>) -> Arc<Session> {
    let mut env: nbmcp::config::Env = [("MCP_TEST_MODE".to_string(), "1".to_string())].into_iter().collect();
    // Des valeurs d'apparence réelle sont posées exprès : le mode test ne doit ni les lire ni les utiliser.
    env.insert("NETBOX_URL".into(), "https://netbox.prod.example.com".into());
    env.insert("NETBOX_TOKEN".into(), "nbt_AAAA.realLookingToken".into());
    if let Some(a) = approver {
        env.insert("MCP_TEST_APPROVER".into(), a.into());
    }
    Session::new(Registry::new(Runtime::new(load_config(&env).unwrap()).await.unwrap()), None, None)
}

async fn rpc(s: &Arc<Session>, method: &str, params: Value) -> Value {
    s.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })).await.unwrap()
}

/// (isError, texte brut, JSON du texte)
async fn call(s: &Arc<Session>, name: &str, args: &Value) -> (bool, String, Value) {
    let r = rpc(s, "tools/call", json!({ "name": name, "arguments": args })).await;
    let res = &r["result"];
    assert!(r.get("error").is_none(), "{name}: protocol error {r}");
    let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
    let parsed = serde_json::from_str(&text).unwrap_or(Value::Null);
    (res["isError"] == true, text, parsed)
}

async fn listed(s: &Arc<Session>) -> Vec<Value> {
    rpc(s, "tools/list", json!({})).await["result"]["tools"].as_array().unwrap().clone()
}

async fn stats(s: &Arc<Session>) -> Value {
    call(s, "netbox_stats", &json!({})).await.2
}

#[tokio::test]
async fn test_mode_lists_every_tool_and_has_a_call_for_each() {
    let s = session(None).await;
    let tools = listed(&s).await;
    let mut names: Vec<String> = tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    names.sort();
    let mut wanted: Vec<String> = calls().keys().cloned().collect();
    wanted.sort();
    assert_eq!(names.len(), TOOL_COUNT);
    assert_eq!(names, wanted, "test/fixtures/calls.json must hold one call per tool");
    // Rien dans les outils ne permet de sortir du mode test ou de choisir l'approbateur.
    for t in &tools {
        let props = t["inputSchema"]["properties"].to_string();
        assert!(!props.contains("test_mode") && !props.contains("approver"), "{}", t["name"]);
    }
}

#[tokio::test]
async fn proof_a_every_tool_answers_from_the_mock_without_touching_the_real_transport() {
    let s = session(Some("approve")).await;
    for (name, args) in calls() {
        let (err, text, v) = call(&s, &name, &args).await;
        assert!(!err, "{name}: {text}");
        assert_eq!(v["test_mode"], true, "{name} is not marked: {text}");
        assert!(v["test_notice"].as_str().unwrap().starts_with("[TEST]"), "{name}");
    }
    let st = stats(&s).await;
    assert_eq!(st["real_calls"], 0);
    assert_eq!(st["egress_blocked"], 0);
    assert_eq!(st["errors"], 0);
    assert_eq!(st["validations"]["approved"], 10);
    assert!(st["mock_writes"].as_u64().unwrap() >= 10, "the approved writes ran against the mock: {st}");
}

#[tokio::test]
async fn validation_four_scenarios_for_every_tool_that_needs_it() {
    let probe = session(None).await;
    let confirming: Vec<String> = listed(&probe).await.iter().filter(|t| t["annotations"]["readOnlyHint"] == false).map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(confirming.len(), 10, "{confirming:?}");
    let all = calls();
    // (approbateur, type d'erreur attendu ou None si exécuté)
    let scenarios = [("approve", None), ("deny", Some("user_cancelled")), ("timeout", Some("timeout")), ("absent", Some("confirmation_required"))];
    for (approver, expected) in scenarios {
        let s = session(Some(approver)).await;
        for name in &confirming {
            let before = stats(&s).await["mock_writes"].as_u64().unwrap();
            let (err, text, v) = call(&s, name, &all[name]).await;
            let after = stats(&s).await["mock_writes"].as_u64().unwrap();
            assert_eq!(v["test_mode"], true, "{approver}/{name}: unmarked: {text}");
            match expected {
                None => {
                    assert!(!err, "{approver}/{name}: {text}");
                    assert!(after > before, "{approver}/{name}: the mock did not run");
                }
                Some(kind) => {
                    assert!(err && v["error_type"] == kind, "{approver}/{name}: expected {kind}, got {text}");
                    assert_eq!(after, before, "{approver}/{name}: something ran after a refusal");
                }
            }
        }
        let st = stats(&s).await;
        let key = match approver {
            "approve" => "approved",
            "deny" => "denied",
            "timeout" => "timed_out",
            _ => "no_confirmation_possible",
        };
        assert_eq!(st["validations"][key], 10, "{approver}: {st}");
        assert_eq!(st["real_calls"], 0);
        assert_eq!(st["egress_blocked"], 0);
    }
}

#[tokio::test]
async fn the_approver_is_closed_by_default_and_rejects_unknown_values() {
    let s = session(None).await;
    let all = calls();
    let (err, text, _) = call(&s, "netbox_delete_object", &all["netbox_delete_object"]).await;
    assert!(err && text.contains("user_cancelled"), "{text}");
    let env: nbmcp::config::Env = [("MCP_TEST_MODE", "1"), ("MCP_TEST_APPROVER", "yes-please")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert!(load_config(&env).is_err());
}

#[test]
fn the_approver_does_not_exist_outside_test_mode() {
    let env: nbmcp::config::Env = [("NETBOX_URL", "http://n"), ("NETBOX_TOKEN", "t"), ("MCP_TEST_APPROVER", "approve")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let c = load_config(&env).unwrap();
    assert!(!c.test_mode && c.approver.is_none());
}

// ---------- preuve B : le vrai binaire, un écouteur sentinelle, isolement réseau ----------

struct Sentinel {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Sentinel {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let (hits, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let addr = l.local_addr().unwrap();
        let (h, st) = (hits.clone(), stop.clone());
        std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                if l.accept().is_ok() {
                    h.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Sentinel { addr, hits, stop }
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::Relaxed)
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Stdio2 {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    next: i64,
}

impl Stdio2 {
    fn start(sentinel: &Sentinel) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nbmcp"))
            .env("MCP_TEST_MODE", "1")
            .env("MCP_TEST_APPROVER", "approve")
            .env("NETBOX_URL", format!("http://{}", sentinel.addr))
            .env("NETBOX_TOKEN", "nbt_AAAA.realLookingToken")
            .env_remove("NETBOX_ALLOW_WRITES")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Stdio2 { child, out, next: 1 }
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        let mut resp = String::new();
        self.out.read_line(&mut resp).unwrap();
        serde_json::from_str(&resp).unwrap_or_else(|_| panic!("no JSON answer for {method}: {resp:?}"))
    }
}

impl Drop for Stdio2 {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn proof_b_binary_all_tools_zero_connection_to_the_sentinel() {
    let sentinel = Sentinel::start();
    let mut srv = Stdio2::start(&sentinel);
    srv.rpc("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } }));
    let listed = srv.rpc("tools/list", json!({}));
    let names: Vec<String> = listed["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names.len(), TOOL_COUNT, "{names:?}");
    for name in &names {
        let args = calls().remove(name).unwrap_or_else(|| panic!("no fixture call for {name}"));
        let r = srv.rpc("tools/call", json!({ "name": name, "arguments": args }));
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(r["result"]["isError"] != true, "{name}: {text}");
        let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        assert_eq!(v["test_mode"], true, "{name} answered without the test marker: {text}");
    }
    let st = srv.rpc("tools/call", json!({ "name": "netbox_stats", "arguments": {} }));
    let st: Value = serde_json::from_str(st["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(st["real_calls"], 0, "{st}");
    assert_eq!(st["egress_blocked"], 0, "{st}");
    assert_eq!(st["test_mode_calls"].as_u64().unwrap(), TOOL_COUNT as u64, "{st}");
    // Le test de fond : l'écouteur sentinelle n'a reçu aucune connexion.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(sentinel.hits(), 0, "the sentinel received a connection: something reached the network in test mode");
}

#[test]
fn proof_b_the_os_isolation_is_real_when_requested() {
    if std::env::var("NBMCP_ISOLATED").as_deref() != Ok("1") {
        eprintln!("NBMCP_ISOLATED is not set: OS network isolation not asserted in this run");
        return;
    }
    // Sous `--network none` / `unshare -n`, aucune adresse publique n'est joignable.
    for target in ["1.1.1.1:443", "8.8.8.8:53"] {
        let addr: SocketAddr = target.parse().unwrap();
        assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_err(), "{target} is reachable: the run is not network-isolated");
    }
}
