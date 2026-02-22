//! Routing helpers for entity-prefixed IDs.
//!
//! SOVD gateways and proxies prefix resource IDs (parameters, operations,
//! outputs, faults) with a sub-entity identifier separated by `/`.
//! These helpers centralise the prefix logic so every call site behaves
//! identically.

/// Strip `"prefix/"` from an ID.  Returns `None` if the ID does not
/// start with the given prefix.
///
/// Used by proxy list methods to filter items belonging to a sub-entity.
///
/// ```
/// # use sovd_core::routing::strip_entity_prefix;
/// assert_eq!(strip_entity_prefix("ecu/rpm", "ecu"), Some("rpm".to_string()));
/// assert_eq!(strip_entity_prefix("other/rpm", "ecu"), None);
/// assert_eq!(strip_entity_prefix("rpm", "ecu"), None);
/// ```
pub fn strip_entity_prefix(id: &str, prefix: &str) -> Option<String> {
    let full = format!("{}/", prefix);
    if id.starts_with(&full) {
        Some(id[full.len()..].to_string())
    } else {
        None
    }
}

/// Prepend `"prefix/"` to an ID, or return the ID unchanged when
/// `prefix` is `None`.
///
/// Used by proxy get/action methods (to send prefixed IDs upstream)
/// and gateway list methods (to expose prefixed IDs downstream).
///
/// ```
/// # use sovd_core::routing::prefixed_id;
/// assert_eq!(prefixed_id("rpm", Some("ecu")), "ecu/rpm");
/// assert_eq!(prefixed_id("rpm", None), "rpm");
/// ```
pub fn prefixed_id(id: &str, prefix: Option<&str>) -> String {
    match prefix {
        Some(pfx) => format!("{}/{}", pfx, id),
        None => id.to_string(),
    }
}

/// Split `"child/local_id"` into `("child", "local_id")`.
///
/// Returns `None` when there is no `/` in the string.
/// Used by gateway routing to find the backend that owns a resource.
///
/// ```
/// # use sovd_core::routing::split_entity_prefix;
/// assert_eq!(split_entity_prefix("ecu/rpm"), Some(("ecu", "rpm")));
/// assert_eq!(split_entity_prefix("rpm"), None);
/// ```
pub fn split_entity_prefix(id: &str) -> Option<(&str, &str)> {
    let idx = id.find('/')?;
    Some((&id[..idx], &id[idx + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_matching_prefix() {
        assert_eq!(
            strip_entity_prefix("engine_ecu/boost_pressure", "engine_ecu"),
            Some("boost_pressure".to_string())
        );
    }

    #[test]
    fn strip_non_matching_prefix() {
        assert_eq!(
            strip_entity_prefix("other_ecu/boost_pressure", "engine_ecu"),
            None
        );
    }

    #[test]
    fn strip_no_prefix() {
        assert_eq!(strip_entity_prefix("boost_pressure", "engine_ecu"), None);
    }

    #[test]
    fn prefixed_with_some() {
        assert_eq!(
            prefixed_id("boost_pressure", Some("engine_ecu")),
            "engine_ecu/boost_pressure"
        );
    }

    #[test]
    fn prefixed_with_none() {
        assert_eq!(prefixed_id("boost_pressure", None), "boost_pressure");
    }

    #[test]
    fn split_with_prefix() {
        assert_eq!(
            split_entity_prefix("engine_ecu/boost_pressure"),
            Some(("engine_ecu", "boost_pressure"))
        );
    }

    #[test]
    fn split_without_prefix() {
        assert_eq!(split_entity_prefix("boost_pressure"), None);
    }

    #[test]
    fn split_nested_prefix() {
        // Only splits on the first `/`
        assert_eq!(
            split_entity_prefix("gateway/ecu/param"),
            Some(("gateway", "ecu/param"))
        );
    }
}
