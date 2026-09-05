//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign keys depend on the unique index selected at constraint creation.

use super::{CatalogIndexRow, Engine, SQLError};

pub(super) fn dependents(
    engine: &Engine,
    indexes: &[CatalogIndexRow],
    cascade: bool,
) -> Result<Vec<(String, String)>, SQLError> {
    let mut dependents = std::collections::BTreeSet::new();
    for index in indexes {
        for (table, foreign_key) in engine
            .try_referrers_to(&index.table_name)
            .map_err(|error| {
                SQLError::Internal(format!("index foreign-key dependencies: {error}"))
            })?
        {
            if foreign_key.referenced_key.as_deref() != Some(index.relation.name.as_str()) {
                continue;
            }
            let name = foreign_key
                .name
                .ok_or_else(|| SQLError::Internal("unnamed foreign-key dependency".into()))?;
            if !cascade {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!("cannot drop index {} because constraint {name} on table {table} depends on it", index.relation.name),
                });
            }
            dependents.insert((table, name));
        }
    }
    Ok(dependents.into_iter().collect())
}
