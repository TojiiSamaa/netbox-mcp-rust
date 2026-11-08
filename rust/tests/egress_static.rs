//! Preuve A (statique) : aucun appel réseau, SSH ou processus n'existe hors du point unique `src/client.rs`.
//! Si quelqu'un ajoute un appel réel ailleurs dans `src/`, ce test échoue (voir scripts/mutation-check.sh).

use std::fs;
use std::path::Path;

/// Motifs d'E/S réseau ou de processus. Autorisés dans `client.rs` seulement.
const FORBIDDEN: &[&str] = &[
    "reqwest::Client",
    "reqwest::get",
    "reqwest::blocking",
    "reqwest::Request",
    "reqwest::ClientBuilder",
    "TcpStream",
    "UdpSocket",
    "UnixStream",
    "lookup_host",
    "ToSocketAddrs",
    "hyper::client",
    "hyper_util",
    "ureq",
    "isahc",
    "surf::",
    "std::process::Command",
    "tokio::process",
    "Command::new",
    ".send().await",
];

/// Écoute entrante du serveur MCP lui-même (pas une sortie) : seul `http.rs`, seul `TcpListener`.
const INGRESS_FILE: &str = "http.rs";
const INGRESS_ALLOWED: &[&str] = &["tokio::net::TcpListener", "TcpListener"];

fn sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") {
            out.push((p.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&p).unwrap()));
        }
    }
    out
}

/// Code hors commentaires de ligne et hors tests unitaires (`#[cfg(test)]`).
fn code_of(text: &str) -> String {
    let body = text.split("#[cfg(test)]").next().unwrap_or(text);
    body.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_no_network_or_process_call_outside_the_single_point() {
    let mut offenders = Vec::new();
    for (name, text) in sources() {
        if name == "client.rs" {
            continue;
        }
        let code = code_of(&text);
        for pat in FORBIDDEN {
            if code.contains(pat) {
                offenders.push(format!("{name}: `{pat}`"));
            }
        }
        // `reqwest` ne sert hors client.rs qu'à nommer des méthodes HTTP.
        for (i, _) in code.match_indices("reqwest::") {
            let rest = &code[i + 9..];
            if !rest.starts_with("Method") && !rest.starts_with("StatusCode") {
                offenders.push(format!("{name}: `reqwest::{}`", rest.chars().take(12).collect::<String>()));
            }
        }
        if name != INGRESS_FILE && INGRESS_ALLOWED.iter().any(|p| code.contains(p)) {
            offenders.push(format!("{name}: a listener outside {INGRESS_FILE}"));
        }
    }
    assert!(offenders.is_empty(), "network/process I/O found outside src/client.rs: {offenders:?}");
}

#[test]
fn a_the_single_point_has_exactly_one_real_send_and_it_is_guarded() {
    let client = sources().into_iter().find(|(n, _)| n == "client.rs").unwrap().1;
    let code = code_of(&client);
    assert_eq!(code.matches(".send().await").count(), 1, "client.rs must hold exactly one real send");
    // L'envoi réel n'est atteignable que si aucun mock n'est installé (mode test), sinon `Tripwire` panique.
    let mock_branch = code.find("if let Some(mock) = &self.mock").expect("test-mode branch");
    let real_send = code.find(".send().await").unwrap();
    assert!(mock_branch < real_send, "the mock branch must come before the real send");
    assert!(code.contains("Transport::Tripwire"), "test mode must build the Tripwire transport, never the real client");
}
