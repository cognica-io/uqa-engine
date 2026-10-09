//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The identity attributes `information_schema.columns` reads from the sequence a column owns.

use super::super::helpers::rows::str_value;
use uqa_core::Value;
use uqa_sql::ast::ColumnDef as SQLColumnDef;
use uqa_sql::SQLError;

/// The identity attributes of a column, which `PostgreSQL` reads from the sequence the column owns. A partition's identity column draws from its parent's sequence, which it does not own, so like any other column it shows none.
pub(super) struct IdentityAttributes {
    pub(super) start: Value,
    pub(super) increment: Value,
    pub(super) maximum: Value,
    pub(super) minimum: Value,
    pub(super) cycle: Value,
}

impl IdentityAttributes {
    pub(super) fn of(
        column: &SQLColumnDef,
        sequence: Option<&crate::catalog::sequence::SequenceState>,
    ) -> Self {
        match sequence {
            Some(state) => Self {
                start: str_value(state.start.to_string()),
                increment: str_value(state.increment.to_string()),
                maximum: str_value(state.max_value.to_string()),
                minimum: str_value(state.min_value.to_string()),
                cycle: str_value(if state.cycle { "YES" } else { "NO" }),
            },
            // A column an earlier version gave a table counter instead of a sequence counts from one by one.
            None if column
                .auto_increment
                .as_ref()
                .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy) =>
            {
                Self {
                    start: str_value("1"),
                    increment: str_value("1"),
                    maximum: Value::Null,
                    minimum: Value::Null,
                    cycle: str_value("NO"),
                }
            }
            None => Self {
                start: Value::Null,
                increment: Value::Null,
                maximum: Value::Null,
                minimum: Value::Null,
                cycle: str_value("NO"),
            },
        }
    }
}

/// Borrow only the definition of the identity sequence owned by this visible column.
pub(super) fn owned_identity_sequence<'a>(
    catalog: &'a crate::catalog::CatalogReadView,
    request: &super::super::CatalogRequest,
    table: &str,
    column: &SQLColumnDef,
) -> Result<Option<&'a crate::catalog::sequence::SequenceState>, SQLError> {
    if ![
        "identity_start",
        "identity_increment",
        "identity_maximum",
        "identity_minimum",
        "identity_cycle",
    ]
    .iter()
    .any(|field| request.includes(field))
    {
        return Ok(None);
    }
    let Some(provenance) = column
        .auto_increment
        .as_ref()
        .filter(|provenance| provenance.is_identity())
    else {
        return Ok(None);
    };
    let (Some(sequence), Some(owner)) = (provenance.sequence.as_ref(), provenance.owner.as_ref())
    else {
        return Ok(None);
    };
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?;
    if !uqa_sql::schema::sequences::implicit::stored_owner_names_current(&relation, column, owner) {
        return Ok(None);
    }
    let Ok(identity) = uqa_core::RelationIdentity::from_legacy_name(sequence) else {
        return Ok(None);
    };
    // Stored provenance names are canonical keys, not names resolved through a search path.
    if identity.qualified_name() != *sequence {
        return Ok(None);
    }
    Ok(catalog.snapshot().definitions.sequences.get(&identity))
}

#[cfg(test)]
mod tests;
