//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict, alias-aware catalog parameter parsing.

use std::collections::BTreeMap;

use crate::{StorageBackendError, StorageBackendResult};

pub(super) fn read_positive_usize(
    parameters: &BTreeMap<String, String>,
    keys: &[&str],
    default: usize,
    index_kind: &str,
) -> StorageBackendResult<usize> {
    let Some((key, raw)) = find_parameter(parameters, keys, index_kind)? else {
        return Ok(default);
    };
    let value = raw.parse::<usize>().map_err(|_| {
        StorageBackendError::Other(format!(
            "invalid persisted {index_kind} parameter `{key}` value `{raw}`"
        ))
    })?;
    if value == 0 {
        return Err(StorageBackendError::Other(format!(
            "persisted {index_kind} parameter `{key}` must be greater than zero"
        )));
    }
    Ok(value)
}

pub(super) fn read_u64(
    parameters: &BTreeMap<String, String>,
    keys: &[&str],
    default: u64,
    index_kind: &str,
) -> StorageBackendResult<u64> {
    let Some((key, raw)) = find_parameter(parameters, keys, index_kind)? else {
        return Ok(default);
    };
    raw.parse::<u64>().map_err(|_| {
        StorageBackendError::Other(format!(
            "invalid persisted {index_kind} parameter `{key}` value `{raw}`"
        ))
    })
}

fn find_parameter<'a>(
    parameters: &'a BTreeMap<String, String>,
    keys: &[&str],
    index_kind: &str,
) -> StorageBackendResult<Option<(&'a str, &'a str)>> {
    let mut found = None;
    for (key, value) in parameters {
        if !keys
            .iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
        {
            continue;
        }
        if let Some((existing, _)) = found {
            return Err(StorageBackendError::Other(format!(
                "duplicate persisted {index_kind} parameters `{existing}` and `{key}`"
            )));
        }
        found = Some((key.as_str(), value.as_str()));
    }
    Ok(found)
}

pub(super) fn reject_unknown_parameters(
    parameters: &BTreeMap<String, String>,
    allowed: &[&str],
    index_kind: &str,
) -> StorageBackendResult<()> {
    if let Some(key) = parameters.keys().find(|key| {
        !allowed
            .iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
    }) {
        return Err(StorageBackendError::Other(format!(
            "unsupported persisted {index_kind} parameter `{key}`"
        )));
    }
    Ok(())
}
