//! Approbation hors bande (`NETBOX_APPROVAL_DIR`) : le modèle ne détient rien qui puisse approuver.
//! Un appel qui demande confirmation et qui ne peut pas la recevoir (HTTP, client sans elicitation) est refusé
//! avec un code à usage unique. L'opérateur l'approuve dans SON terminal (`nbmcp approve <code>`, qui montre
//! l'action exacte) ; le même appel, répété à l'identique, passe alors une fois. Le code ne sert qu'à nommer la
//! demande : le connaître n'approuve rien, c'est le fichier écrit par la commande qui approuve.
//! Limite : le dossier doit être hors de portée de l'agent (autre compte, ou hors de son bac à sable) ;
//! un agent qui peut écrire dedans peut s'approuver lui-même.

use serde_json::{json, Value};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Durée de vie d'une demande, approuvée ou non.
const TTL_SECS: u64 = 600;

#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Approuvée par l'opérateur : consommée, valable pour cet appel seulement.
    Approved,
    /// En attente : code à donner à l'opérateur.
    Pending(String),
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 10 caractères tirés de l'aléa du système (graines de `RandomState`), sans dépendance de plus.
fn new_code() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut h = RandomState::new().build_hasher();
    h.write_u64(now());
    let (mut n, mut out) = (h.finish() as u128 * 0x1_0000_0001 + RandomState::new().build_hasher().finish() as u128, String::new());
    for _ in 0..10 {
        out.push(ALPHABET[(n % ALPHABET.len() as u128) as usize] as char);
        n /= ALPHABET.len() as u128;
    }
    out
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write(dir: &Path, code: &str, v: &Value) -> std::io::Result<()> {
    let tmp = dir.join(format!("{code}.tmp"));
    std::fs::write(&tmp, v.to_string())?;
    std::fs::rename(tmp, dir.join(format!("{code}.json")))
}

fn make_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Les demandes vivantes du dossier : (code, contenu). Les périmées sont supprimées au passage.
fn live(dir: &Path, at: u64) -> Vec<(String, Value)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut found = vec![];
    for e in entries.flatten() {
        let path = e.path();
        let (Some("json"), Some(code)) = (path.extension().and_then(|x| x.to_str()), path.file_stem().and_then(|x| x.to_str())) else { continue };
        match read(&path) {
            Some(v) if v["expires"].as_u64().is_some_and(|x| x >= at) => found.push((code.to_string(), v)),
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    found
}

/// Cet appel (outil + arguments, ordre des clés sans effet) est-il approuvé ? Sinon, sous quel code attend-il ?
pub fn check(dir: &Path, tool: &str, args: &Value, message: &str) -> std::io::Result<Verdict> {
    check_at(dir, tool, args, message, now())
}

pub fn check_at(dir: &Path, tool: &str, args: &Value, message: &str, at: u64) -> std::io::Result<Verdict> {
    make_dir(dir)?;
    let mut waiting = None;
    for (code, v) in live(dir, at) {
        if v["tool"] != tool || &v["args"] != args {
            continue;
        }
        if v["approved"] == true {
            std::fs::remove_file(dir.join(format!("{code}.json")))?;
            return Ok(Verdict::Approved);
        }
        waiting = Some(code);
    }
    if let Some(code) = waiting {
        return Ok(Verdict::Pending(code));
    }
    let code = new_code();
    write(dir, &code, &json!({ "tool": tool, "args": args, "message": message, "expires": at + TTL_SECS, "approved": false }))?;
    Ok(Verdict::Pending(code))
}

/// Les demandes en attente d'approbation : (code, action).
pub fn pending(dir: &Path) -> Vec<(String, String)> {
    live(dir, now()).into_iter().filter(|(_, v)| v["approved"] != true).map(|(c, v)| (c, v["message"].as_str().unwrap_or("").to_string())).collect()
}

/// Approuve une demande en attente ; rend l'action approuvée. C'est la seule écriture qui approuve.
pub fn approve(dir: &Path, code: &str) -> Result<String, String> {
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("\"{code}\" is not an approval code."));
    }
    let path = dir.join(format!("{code}.json"));
    let mut v = read(&path).filter(|v| v["expires"].as_u64().is_some_and(|x| x >= now())).ok_or_else(|| format!("No pending request {code} (unknown, expired or already used)."))?;
    let message = v["message"].as_str().unwrap_or("").to_string();
    v["approved"] = json!(true);
    write(dir, code, &v).map_err(|e| format!("Could not record the approval: {e}"))?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("nbmcp-approval-{name}-{}", new_code()))
    }

    #[test]
    fn a_request_is_approved_once_and_only_for_the_same_call() {
        let d = dir("once");
        let args = json!({ "object_type": "dcim.site", "object_id": 1 });
        let Verdict::Pending(code) = check(&d, "netbox_delete_object", &args, "Delete site 1?").unwrap() else { panic!("expected a pending request") };
        // Même appel, toujours pas approuvé : même code, pas de doublon.
        assert_eq!(check(&d, "netbox_delete_object", &args, "Delete site 1?").unwrap(), Verdict::Pending(code.clone()));
        assert_eq!(pending(&d), vec![(code.clone(), "Delete site 1?".to_string())]);
        assert_eq!(approve(&d, &code).unwrap(), "Delete site 1?");
        // Un autre objet ou un autre outil n'hérite pas de l'approbation.
        assert!(matches!(check(&d, "netbox_delete_object", &json!({ "object_type": "dcim.site", "object_id": 2 }), "m").unwrap(), Verdict::Pending(_)));
        assert!(matches!(check(&d, "netbox_bulk_delete", &args, "m").unwrap(), Verdict::Pending(_)));
        // L'ordre des clés ne compte pas ; l'approbation est consommée par le premier passage.
        let reordered = json!({ "object_id": 1, "object_type": "dcim.site" });
        assert_eq!(check(&d, "netbox_delete_object", &reordered, "m").unwrap(), Verdict::Approved);
        assert!(matches!(check(&d, "netbox_delete_object", &args, "m").unwrap(), Verdict::Pending(_)));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn requests_expire_and_codes_cannot_escape_the_directory() {
        let d = dir("expiry");
        let args = json!({ "x": 1 });
        let Verdict::Pending(code) = check_at(&d, "t", &args, "m", 1000).unwrap() else { panic!("expected a pending request") };
        // Périmée : approuver échoue (le code est inconnu pour `approve` à l'heure réelle) et un nouvel appel a un nouveau code.
        let Verdict::Pending(later) = check_at(&d, "t", &args, "m", 1000 + TTL_SECS + 1).unwrap() else { panic!("expected a pending request") };
        assert_ne!(code, later);
        assert!(approve(&d, "../x").is_err() && approve(&d, "").is_err() && approve(&d, "unknown").is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
