//! Client REST NetBox. Une seule `reqwest::Client` partagée ; le jeton peut changer par requête (passthrough).
//! Les redirections ne sont pas suivies : un POST redirigé http→https perd son corps et l'écriture
//! échouerait sans bruit, mieux vaut dire à l'opérateur de corriger NETBOX_URL.

use crate::config::Config;
use crate::error::{ErrorType, NbError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::time::Duration;

#[derive(Clone)]
pub struct NetBox {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

/// Normalise un chemin d'API : `dcim/devices`, `/api/dcim/devices/` et `dcim/devices/` donnent `dcim/devices`.
/// Refuse tout ce qui sortirait de `/api` (schéma, `..`, requête, ancre, antislash).
pub fn api_path(raw: &str) -> Result<String> {
    let bad = |why: &str| {
        NbError::new(ErrorType::InvalidInput, format!("Invalid path \"{raw}\": {why}."))
            .hint("Use a path relative to /api, for example dcim/devices/12/trace. Put filters in `query`.")
    };
    if raw.contains("://") || raw.contains("..") || raw.contains('\\') || raw.contains('?') || raw.contains('#') {
        return Err(bad("no scheme, '..', '?', '#' or backslash allowed"));
    }
    if raw.chars().any(|c| c.is_control() || c == ' ') {
        return Err(bad("no space or control character allowed"));
    }
    let p = raw.trim().trim_matches('/');
    let p = p.strip_prefix("api/").unwrap_or(p);
    if p.is_empty() || p == "api" {
        return Err(bad("empty path"));
    }
    if p.contains("//") {
        return Err(bad("empty segment"));
    }
    Ok(p.to_string())
}

/// Un objet de filtres devient des paires `clé=valeur` ; un tableau répète la clé (`id=1&id=2`).
pub fn query_pairs(map: &Map<String, Value>) -> Vec<(String, String)> {
    fn one(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }
    let mut out = Vec::new();
    for (k, v) in map {
        match v {
            Value::Null => {}
            Value::Array(a) => out.extend(a.iter().filter_map(one).map(|s| (k.clone(), s))),
            other => out.extend(one(other).map(|s| (k.clone(), s))),
        }
    }
    out
}

impl NetBox {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(!cfg.verify_ssl)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("nbmcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| NbError::new(ErrorType::ConfigError, format!("Cannot build the HTTP client: {e}")))?;
        Ok(NetBox { http, base: cfg.netbox_url.clone(), token: cfg.token.clone() })
    }

    pub fn with_token(&self, token: String) -> Self {
        NetBox { token: Some(token), ..self.clone() }
    }

    fn auth(&self) -> Result<String> {
        let t = self.token.as_deref().ok_or_else(|| {
            NbError::new(ErrorType::AuthFailed, "No NetBox token for this request.")
                .hint("Send `Authorization: Bearer <netbox token>` (NETBOX_TOKEN_PASSTHROUGH=true).")
        })?;
        // v2 : `nbt_<clé>.<jeton>` en Bearer ; v1 : jeton brut en `Token`.
        Ok(if t.starts_with("nbt_") { format!("Bearer {t}") } else { format!("Token {t}") })
    }

    /// `path` est relatif à `/api` (voir `api_path`).
    pub async fn request(&self, method: Method, path: &str, query: &[(String, String)], body: Option<&Value>) -> Result<Value> {
        self.send(method, format!("{}/api/{}/", self.base, path.trim_matches('/')), query, body).await
    }

    pub async fn get(&self, path: &str, query: &[(String, String)]) -> Result<Value> {
        self.request(Method::GET, path, query, None).await
    }

    /// GET avec un second chemin essayé si le premier répond 404 (renommages d'une version de NetBox à l'autre).
    /// Une 404 JSON (donc avec `details`) vient d'un objet absent sur un chemin qui existe : pas de repli.
    pub async fn get_with_fallback(&self, path: &str, fallback: Option<&str>, query: &[(String, String)]) -> Result<Value> {
        match (self.get(path, query).await, fallback) {
            (Err(e), Some(fb)) if e.is(ErrorType::NotFound) && e.details.is_none() => self.get(fb, query).await,
            (r, _) => r,
        }
    }

    /// GraphQL vit à `/graphql/`, hors de `/api`.
    pub async fn graphql(&self, query: &str, variables: Option<&Value>) -> Result<Value> {
        let mut body = json!({ "query": query });
        if let Some(v) = variables {
            body["variables"] = v.clone();
        }
        self.send(Method::POST, format!("{}/graphql/", self.base), &[], Some(&body)).await
    }

    async fn send(&self, method: Method, url: String, query: &[(String, String)], body: Option<&Value>) -> Result<Value> {
        let mut req = self
            .http
            .request(method.clone(), &url)
            .header("Authorization", self.auth()?)
            .header("Accept", "application/json")
            .query(query);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| self.transport_error(&e))?;
        let status = resp.status();
        let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).map(String::from);
        let text = resp.text().await.map_err(|e| self.transport_error(&e))?;
        if status.is_redirection() {
            return Err(NbError::new(
                ErrorType::BadResponse,
                format!("NetBox redirected the request to {}.", location.as_deref().unwrap_or("another address")),
            )
            .status(status.as_u16())
            .hint("Set NETBOX_URL to the address NetBox redirects to (usually https://). Redirects are not followed: a redirected write would lose its body."));
        }
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        if status.is_success() {
            return match (status.as_u16(), parsed) {
                (204, _) => Ok(Value::Null),
                (_, Some(v)) => Ok(v),
                (_, None) if text.trim().is_empty() => Ok(Value::Null),
                (_, None) => Err(not_json(&text, status.as_u16())),
            };
        }
        Err(http_error(status.as_u16(), parsed, &text, &method, &url))
    }

    fn transport_error(&self, e: &reqwest::Error) -> NbError {
        let msg = format!("{e:?}");
        if e.is_timeout() {
            NbError::new(ErrorType::Timeout, "NetBox did not answer in time.")
                .hint("Raise NETBOX_TIMEOUT, or narrow the query (filters, limit, fields).")
        } else if msg.to_ascii_lowercase().contains("certificate") {
            NbError::new(ErrorType::TlsError, "TLS certificate check failed.")
                .hint("Fix the certificate, or set VERIFY_SSL=false for a lab (insecure).")
        } else {
            // Pas de `e.url()` ni de `Display` complet : l'adresse peut porter des paramètres.
            NbError::new(ErrorType::Unreachable, format!("Cannot reach NetBox at {}.", self.base))
        }
    }
}

fn not_json(text: &str, status: u16) -> NbError {
    let html = text.trim_start().starts_with('<');
    NbError::new(
        ErrorType::BadResponse,
        format!("NetBox answered {status} with {}.", if html { "an HTML page" } else { "something that is not JSON" }),
    )
    .hint(if html { "NETBOX_URL probably points to a login page or a proxy, not to NetBox." } else { "Check NETBOX_URL." })
    .details(json!({ "body_start": text.chars().take(200).collect::<String>() }))
}

fn http_error(status: u16, body: Option<Value>, text: &str, method: &Method, url: &str) -> NbError {
    let detail = body.as_ref().and_then(|b| b.get("detail")).and_then(Value::as_str).map(String::from);
    let shown = detail.unwrap_or_else(|| text.chars().take(300).collect());
    let at = url.split("/api/").nth(1).unwrap_or(url);
    let (kind, hint): (ErrorType, &str) = match status {
        400 => (ErrorType::ValidationFailed, "NetBox rejected the values; `details` holds the error per field. Do not resend the same payload: call netbox_describe_type for field names, required fields and choices."),
        401 => (ErrorType::AuthFailed, "The NetBox token is invalid, expired or has the wrong scheme."),
        403 => (ErrorType::Forbidden, "The token has no permission for this object or action in NetBox. Do not retry: a NetBox admin has to grant it (object permissions, or a write-enabled token)."),
        404 => (ErrorType::NotFound, "Nothing at this address, or no such object. Check the type and the id."),
        405 => (ErrorType::BadRequest, "This endpoint does not support that method."),
        409 => (ErrorType::Conflict, "The object is in use or conflicts with another one."),
        s if s >= 500 => (ErrorType::UpstreamError, "NetBox failed. Retrying later may work."),
        _ => (ErrorType::BadRequest, ""),
    };
    let mut e = NbError::new(kind, format!("NetBox {method} {at} -> {status}: {shown}")).status(status).hint(hint);
    // Validation (400) et objet protégé (409) portent le détail par champ.
    // Une 404 JSON = objet absent sur un chemin qui existe ; une 404 HTML = chemin inconnu de cette version.
    if matches!(status, 400 | 409) || status == 404 {
        if let Some(b) = body {
            e = e.details(b);
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_normalised_and_confined() {
        assert_eq!(api_path("/api/dcim/devices/").unwrap(), "dcim/devices");
        assert_eq!(api_path("dcim/devices/3/trace").unwrap(), "dcim/devices/3/trace");
        for bad in ["", "/", "http://x/api", "a/../b", "a?b=1", "a b", "a//b", "a\\b", "x#y"] {
            assert!(api_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn arrays_repeat_the_key() {
        let m = json!({ "id": [1, 2], "name": "r", "x": null, "ok": true, "o": {"a": 1} });
        let mut q = query_pairs(m.as_object().unwrap());
        q.sort();
        assert_eq!(
            q,
            vec![("id".into(), "1".into()), ("id".into(), "2".into()), ("name".into(), "r".into()), ("ok".into(), "true".into())]
        );
    }

    #[test]
    fn errors_are_typed() {
        let e = http_error(400, Some(json!({"name": ["required"]})), "", &Method::POST, "https://n/api/dcim/sites/");
        assert!(e.is(ErrorType::ValidationFailed));
        assert_eq!(e.details.unwrap()["name"][0], "required");
        assert!(http_error(403, Some(json!({"detail": "no"})), "", &Method::GET, "u").is(ErrorType::Forbidden));
        assert!(http_error(502, None, "bad gateway", &Method::GET, "u").is(ErrorType::UpstreamError));
    }
}
