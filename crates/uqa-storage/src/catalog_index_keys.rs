//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage preserves opaque SQL expressions; the engine owns their dependencies and rewrites.

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum StoredIndexKey {
    Column(String),
    Expression(serde_json::Map<String, serde_json::Value>),
}

pub(crate) fn references_column(encoded: &str, column: &str) -> serde_json::Result<bool> {
    let keys: Vec<StoredIndexKey> = serde_json::from_str(encoded)?;
    Ok(keys
        .iter()
        .any(|key| matches!(key, StoredIndexKey::Column(name) if name == column)))
}

pub(crate) fn rename_column(
    encoded: &str,
    from: &str,
    to: &str,
) -> serde_json::Result<Option<String>> {
    let mut keys: Vec<StoredIndexKey> = serde_json::from_str(encoded)?;
    let mut changed = false;
    for key in &mut keys {
        if let StoredIndexKey::Column(name) = key {
            if name == from {
                *name = to.into();
                changed = true;
            }
        }
    }
    changed.then(|| serde_json::to_string(&keys)).transpose()
}
