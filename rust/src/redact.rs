//! Les secrets que NetBox renvoie (clés pré-partagées VPN/Wi-Fi, jetons d'API) n'ont rien à faire dans
//! le contexte d'un agent. Remplacés par défaut ; NETBOX_REDACT_SECRETS=false pour tout voir.

use serde_json::Value;

const SECRET_KEYS: [&str; 6] = [
    "preshared_key",
    "auth_psk",
    "psk",
    "password",
    "secret",
    "auth_secret",
];
/// Dans les objets `users.token` seulement : `key` y est le jeton lui-même.
const TOKEN_KEYS: [&str; 2] = ["key", "token"];

pub const MASK: &str = "[redacted]";

pub fn redact(value: &mut Value, tokens: bool) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                let secret = SECRET_KEYS.contains(&k.as_str())
                    || (tokens && TOKEN_KEYS.contains(&k.as_str()));
                if secret && matches!(v, Value::String(s) if !s.is_empty()) {
                    *v = Value::String(MASK.into());
                } else {
                    redact(v, tokens);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|v| redact(v, tokens)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_nested_secrets() {
        let mut v = json!({"results": [{"name": "ike", "preshared_key": "abc", "ok": {"auth_psk": "z", "empty": ""}}]});
        redact(&mut v, false);
        assert_eq!(v["results"][0]["preshared_key"], MASK);
        assert_eq!(v["results"][0]["ok"]["auth_psk"], MASK);
        assert_eq!(v["results"][0]["name"], "ike");
    }

    #[test]
    fn token_keys_only_for_tokens() {
        let mut a = json!({"key": "k1"});
        redact(&mut a, false);
        assert_eq!(a["key"], "k1");
        redact(&mut a, true);
        assert_eq!(a["key"], MASK);
    }
}
