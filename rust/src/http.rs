//! HTTP « streamable » sans session : chaque requête a sa propre `Session` au-dessus du `Registry`.
//! Sans session il n'y a personne à interroger : pas d'elicitation, le client se fie à l'annotation « destructif ».
//! Mode passthrough : le Bearer du client EST son jeton NetBox (NetBox applique alors ses propres permissions).

use crate::error::{ErrorType, NbError, Result};
use crate::server::{Registry, Session};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

pub struct HttpOptions {
    pub host: String,
    pub port: u16,
    /// Jeton Bearer du serveur MCP. Obligatoire hors boucle locale, sauf en passthrough.
    pub token: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub cors_origins: Vec<String>,
    pub passthrough: bool,
}

pub fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

fn token_matches(header: Option<&str>, token: &str) -> bool {
    let given = header.unwrap_or("").as_bytes();
    let wanted = format!("Bearer {token}");
    let wanted = wanted.as_bytes();
    let mut diff = (given.len() ^ wanted.len()) as u8;
    for (i, w) in wanted.iter().enumerate() {
        diff |= given.get(i).copied().unwrap_or(0) ^ w;
    }
    diff == 0
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

struct AppState {
    registry: Arc<Registry>,
    token: Option<String>,
    /// None : écoute hors boucle locale sans NETBOX_MCP_ALLOWED_HOSTS, où un jeton est obligatoire ; le contrôle de Host ne protège alors de rien
    /// et casserait docker -p, un reverse proxy ou un nom DNS.
    allowed: Option<HashSet<String>>,
    cors: Vec<String>,
    passthrough: bool,
}

fn deny(status: StatusCode, message: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], json!({ "error": message }).to_string()).into_response()
}

async fn handle(State(state): State<Arc<AppState>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).map(String::from);
    let cors_ok = origin.as_ref().filter(|o| state.cors.iter().any(|c| c == "*" || c == *o)).cloned();
    let mut resp = route(&state, method, uri, headers, body).await;
    if let Some(o) = cors_ok.and_then(|o| HeaderValue::from_str(&o).ok()) {
        let h = resp.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("authorization, content-type, mcp-protocol-version"));
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST, OPTIONS"));
    }
    resp
}

async fn route(state: &AppState, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    // Vérifier Host arrête le DNS rebinding depuis une page web.
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if state.allowed.as_ref().is_some_and(|a| !a.contains(host)) {
        return deny(StatusCode::FORBIDDEN, "Host not allowed. See NETBOX_MCP_ALLOWED_HOSTS.");
    }
    if method == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    let client_token = bearer(&headers);
    if state.passthrough {
        if client_token.is_none() {
            return deny(StatusCode::UNAUTHORIZED, "Send `Authorization: Bearer <NetBox token>`.");
        }
    } else if let Some(token) = &state.token {
        if !token_matches(headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()), token) {
            return deny(StatusCode::UNAUTHORIZED, "Invalid or missing bearer token.");
        }
    }
    if uri.path() != "/mcp" {
        return deny(StatusCode::NOT_FOUND, "Not found. The endpoint is /mcp.");
    }
    if method != Method::POST {
        return deny(StatusCode::METHOD_NOT_ALLOWED, "This server keeps no session: only POST is supported.");
    }
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/json")], json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }).to_string()).into_response();
    };
    let session = Session::new(state.registry.clone(), None, state.passthrough.then_some(client_token).flatten());
    let reply = match message {
        Value::Array(batch) => {
            let replies: Vec<Value> = futures::future::join_all(batch.into_iter().map(|m| session.handle(m))).await.into_iter().flatten().collect();
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        single => session.handle(single).await,
    };
    match reply {
        Some(r) => ([(header::CONTENT_TYPE, "application/json")], r.to_string()).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

pub async fn serve(registry: Arc<Registry>, options: HttpOptions) -> Result<()> {
    let HttpOptions { host, port, token, allowed_hosts, cors_origins, passthrough } = options;
    if token.is_none() && !passthrough && !is_loopback(&host) {
        return Err(NbError::new(ErrorType::ConfigError, format!("Refusing to listen on {host} without a token: anyone on the network could use your NetBox token."))
            .hint("Set NETBOX_MCP_TOKEN (or MCP_AUTH_TOKEN), or NETBOX_TOKEN_PASSTHROUGH=true, or listen on 127.0.0.1."));
    }
    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|e| NbError::new(ErrorType::ConfigError, format!("Cannot listen on {host}:{port}: {e}")))?;
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let shown = if host.contains(':') { format!("[{host}]") } else { host.clone() };
    let explicit = !allowed_hosts.is_empty();
    let mut allowed: HashSet<String> = allowed_hosts.into_iter().collect();
    for name in [shown.as_str(), "localhost", "127.0.0.1", "[::1]"] {
        if name == shown || is_loopback(&host) {
            allowed.insert(format!("{name}:{bound}"));
        }
    }
    crate::log::info(&format!("listening on http://{shown}:{bound}/mcp"));
    let allowed = (is_loopback(&host) || explicit).then_some(allowed);
    let app = Router::new().fallback(handle).with_state(Arc::new(AppState { registry, token, allowed, cors: cors_origins, passthrough }));
    axum::serve(listener, app).await.map_err(|e| NbError::new(ErrorType::InternalError, format!("HTTP server stopped: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(token_matches(Some("Bearer abc"), "abc"));
        assert!(!token_matches(Some("Bearer abd"), "abc"));
        assert!(!token_matches(Some("Bearer abcd"), "abc"));
        assert!(!token_matches(None, "abc"));
    }

    #[test]
    fn loopback() {
        assert!(is_loopback("127.0.0.1") && is_loopback("::1") && is_loopback("localhost"));
        assert!(!is_loopback("0.0.0.0") && !is_loopback("10.0.0.1"));
    }
}
