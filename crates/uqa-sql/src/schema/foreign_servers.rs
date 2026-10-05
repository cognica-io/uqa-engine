//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` foreign-server declaration options.

use crate::SQLError;
use std::collections::BTreeMap;

/// `transformGenericOptions` checks duplicate names before `optionListToArray` rejects an equals sign in a name.
pub fn creation_options(
    options: &[(String, String)],
) -> Result<BTreeMap<String, String>, SQLError> {
    let mut result = BTreeMap::new();
    for (name, value) in options {
        if result.insert(name.clone(), value.clone()).is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42710".into(),
                message: format!("option \"{name}\" provided more than once"),
            });
        }
    }
    if let Some((name, _)) = options.iter().find(|(name, _)| name.contains('=')) {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("invalid option name \"{name}\": must not contain \"=\""),
        });
    }
    Ok(result)
}
