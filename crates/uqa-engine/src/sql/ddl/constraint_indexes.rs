//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint indexes occupy the same relation namespace as explicit indexes.

use crate::{Engine, RelationIdentity};
use uqa_sql::{
    ast::{TableKeyConstraint, TableKeyConstraintKind},
    SQLError,
};

pub(super) fn name_constraint_indexes(
    engine: &Engine,
    table: &str,
    keys: &mut [TableKeyConstraint],
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let existing = if engine.try_resolve_bound_table_name(table)?.is_some() {
        engine
            .try_key_constraints(table)
            .map_err(|error| SQLError::Internal(error.to_string()))?
    } else {
        Vec::new()
    };
    let mut used = std::collections::BTreeSet::new();
    for key in keys {
        if key.name.is_some() && existing.iter().any(|old| old == key) {
            used.insert(key.name.clone().expect("named constraint"));
            continue;
        }
        if let Some(name) = &key.name {
            if !used.insert(name.clone()) || !available(engine, &relation, name)? {
                return Err(SQLError::Routine {
                    sqlstate: "42P07".into(),
                    message: format!("relation \"{name}\" already exists"),
                });
            }
            continue;
        }
        let suffix = if key.kind == TableKeyConstraintKind::PrimaryKey {
            "pkey"
        } else {
            "key"
        };
        let component = if key.kind == TableKeyConstraintKind::PrimaryKey {
            String::new()
        } else {
            key.columns.join("_")
        };
        for number in 0_u64.. {
            let label = if number == 0 {
                suffix.into()
            } else {
                format!("{suffix}{number}")
            };
            let candidate = object_name(&relation.name, &component, &label);
            if !used.contains(&candidate) && available(engine, &relation, &candidate)? {
                used.insert(candidate.clone());
                key.name = Some(candidate);
                break;
            }
        }
    }
    Ok(())
}

fn available(engine: &Engine, table: &RelationIdentity, name: &str) -> Result<bool, SQLError> {
    if name == table.name {
        return Ok(false);
    }
    Ok(matches!(
        engine.resolve_bound_relation_kind(
            &RelationIdentity::new(&table.schema, name).qualified_name()
        )?,
        crate::engine_capabilities::RelationResolution::MissingRelation
    ))
}

/// `PostgreSQL` reserves the fixed suffix and balances truncation of the two varying name components before clipping at UTF-8 boundaries.
fn object_name(table: &str, columns: &str, label: &str) -> String {
    let mut table_length = table.len();
    let mut column_length = columns.len();
    let overhead = label.len() + 1 + usize::from(!columns.is_empty());
    while table_length + column_length + overhead > 63 {
        if table_length > column_length {
            table_length -= 1;
        } else {
            column_length -= 1;
        }
    }
    while !table.is_char_boundary(table_length) {
        table_length -= 1;
    }
    while !columns.is_char_boundary(column_length) {
        column_length -= 1;
    }
    if columns.is_empty() {
        format!("{}_{label}", &table[..table_length])
    } else {
        format!(
            "{}_{}_{label}",
            &table[..table_length],
            &columns[..column_length]
        )
    }
}
