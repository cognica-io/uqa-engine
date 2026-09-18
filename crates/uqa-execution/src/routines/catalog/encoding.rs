//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned routine definitions distinguish explicit owner ACLs from legacy implicit privileges.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_sql::{ast::CreateFunction, routines::security::migrate_implicit_routine_owner_acl};
use uqa_storage::{StorageBackendError, StorageBackendResult};

type Definitions = BTreeMap<String, Vec<CreateFunction>>;

#[derive(Serialize, Deserialize)]
struct RoutineCatalog {
    routine_catalog_format: u32,
    definitions: Definitions,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredRoutineCatalog {
    Versioned(RoutineCatalog),
    Legacy(Definitions),
}

pub(super) fn encode(definitions: Definitions) -> Result<String, serde_json::Error> {
    serde_json::to_string(&RoutineCatalog {
        routine_catalog_format: 1,
        definitions,
    })
}

pub(crate) fn decode(json: &str) -> StorageBackendResult<(Definitions, bool)> {
    match serde_json::from_str::<StoredRoutineCatalog>(json)? {
        StoredRoutineCatalog::Versioned(catalog) => {
            if catalog.routine_catalog_format != 1 {
                return Err(StorageBackendError::Other(format!(
                    "unknown routine catalog format {}",
                    catalog.routine_catalog_format
                )));
            }
            Ok((catalog.definitions, false))
        }
        StoredRoutineCatalog::Legacy(mut definitions) => {
            for definition in definitions.values_mut().flatten() {
                migrate_implicit_routine_owner_acl(definition);
            }
            Ok((definitions, true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_malformed_routine_formats_are_not_treated_as_legacy() {
        for json in [
            r#"{"routine_catalog_format":2,"definitions":{}}"#,
            r#"{"routine_catalog_format":1}"#,
            r#"{"routine_catalog_format":null,"definitions":{}}"#,
        ] {
            assert!(decode(json).is_err(), "{json}");
        }
        assert!(decode("{}").unwrap().1);
        assert!(!decode(&encode(BTreeMap::new()).unwrap()).unwrap().1);
    }
}
