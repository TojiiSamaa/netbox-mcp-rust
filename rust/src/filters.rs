//! Validation des filtres : refuse ce que NetBox ignorerait en silence (`__in`, traversées multiples)
//! et préparation de la requête de liste (pagination, champs, ordre).

use crate::error::{ErrorType, NbError, Result};
use serde_json::{Map, Value};

const SUFFIXES: [&str; 16] = [
    "n", "ic", "nic", "isw", "nisw", "iew", "niew", "ie", "nie", "empty", "regex", "iregex", "lt",
    "lte", "gt", "gte",
];
/// Paramètres que les outils règlent eux-mêmes ; un filtre du même nom serait écrasé sans le dire.
const RESERVED: [&str; 5] = ["limit", "offset", "fields", "brief", "ordering"];

pub fn validate_filters(filters: &Map<String, Value>) -> Result<()> {
    for name in filters.keys() {
        if RESERVED.contains(&name.as_str()) {
            return Err(NbError::new(
                ErrorType::InvalidInput,
                format!("Invalid filter \"{name}\": it is a tool parameter, not a filter. Pass it as the `{name}` argument."),
            ));
        }
        if name == "q" || !name.contains("__") {
            continue;
        }
        let parts: Vec<&str> = name.split("__").collect();
        if parts.len() == 2 && parts[1] == "in" {
            return Err(NbError::new(
                ErrorType::InvalidInput,
                format!("Invalid filter \"{name}\": the \"__in\" suffix is not supported and NetBox may ignore it silently."),
            )
            .hint(format!("Pass a list instead: {{\"{}\": [1, 2, 3]}}", parts[0])));
        }
        if parts.len() == 2 && SUFFIXES.contains(&parts[1]) {
            continue;
        }
        return Err(NbError::new(
            ErrorType::InvalidInput,
            format!("Invalid filter \"{name}\": multi-hop traversal or unknown lookup suffix."),
        )
        .hint(format!(
            "Use direct fields (site_id, name, status) or a lookup among: {}. For cross-object queries, make two calls.",
            SUFFIXES.join(", ")
        )));
    }
    Ok(())
}

/// Les paires de requête d'une liste : filtres, puis paramètres des outils (qui priment).
pub fn list_query(
    filters: &Map<String, Value>,
    fields: &[String],
    brief: bool,
    limit: i64,
    offset: i64,
    ordering: &[String],
) -> Vec<(String, String)> {
    let mut q = crate::client::query_pairs(filters);
    q.push(("limit".into(), limit.to_string()));
    q.push(("offset".into(), offset.to_string()));
    if !fields.is_empty() {
        q.push(("fields".into(), fields.join(",")));
    }
    if brief {
        q.push(("brief".into(), "1".into()));
    }
    let order: Vec<&str> = ordering
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !order.is_empty() {
        q.push(("ordering".into(), order.join(",")));
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check(v: Value) -> Result<()> {
        validate_filters(v.as_object().unwrap())
    }

    #[test]
    fn accepts_plain_and_lookups() {
        assert!(check(
            json!({"site_id": 1, "name__ic": "sw", "vid__gte": 100, "q": "x", "cf_owner__n": "a"})
        )
        .is_ok());
    }

    #[test]
    fn rejects_in_and_multihop() {
        assert!(check(json!({"id__in": [1, 2]})).is_err());
        assert!(check(json!({"device__site_id": 1})).is_err());
        assert!(check(json!({"name__bogus": 1})).is_err());
        assert!(check(json!({"a__b__c": 1})).is_err());
    }

    #[test]
    fn rejects_reserved_names() {
        assert!(check(json!({"limit": 500})).is_err());
        assert!(check(json!({"ordering": "name"})).is_err());
    }

    #[test]
    fn query_has_pagination_and_fields() {
        let f = json!({"site_id": [1, 2]});
        let q = list_query(
            f.as_object().unwrap(),
            &["id".into(), "name".into()],
            true,
            5,
            10,
            &["-id".into(), " ".into()],
        );
        assert!(
            q.contains(&("site_id".into(), "1".into()))
                && q.contains(&("site_id".into(), "2".into()))
        );
        assert!(q.contains(&("fields".into(), "id,name".into())));
        assert!(q.contains(&("brief".into(), "1".into())));
        assert!(q.contains(&("ordering".into(), "-id".into())));
        assert!(q.contains(&("offset".into(), "10".into())));
    }
}
