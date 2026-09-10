//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint indexes occupy the same relation namespace as explicit indexes.

use crate::{
    ast::{TableKeyConstraint, TableKeyConstraintKind},
    SQLError,
};
use uqa_core::RelationIdentity;

/// Relation-namespace visibility and existing constraint identities, without index mutation.
pub trait IndexNameCatalog {
    fn existing_constraint_keys(&self, table: &str) -> Result<Vec<TableKeyConstraint>, SQLError>;
    fn relation_name_available(&self, qualified_name: &str) -> Result<bool, SQLError>;
}

pub fn name_constraint_indexes(
    catalog: &dyn IndexNameCatalog,
    table: &str,
    keys: &mut [TableKeyConstraint],
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let existing = catalog.existing_constraint_keys(table)?;
    let mut used = std::collections::BTreeSet::new();
    for key in keys {
        if key.name.is_some() && existing.iter().any(|old| old == key) {
            used.insert(key.name.clone().expect("named constraint"));
            continue;
        }
        if let Some(name) = &key.name {
            if !used.insert(name.clone()) || !available(catalog, &relation, name)? {
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
            if !used.contains(&candidate) && available(catalog, &relation, &candidate)? {
                used.insert(candidate.clone());
                key.name = Some(candidate);
                break;
            }
        }
    }
    Ok(())
}

fn available(
    catalog: &dyn IndexNameCatalog,
    table: &RelationIdentity,
    name: &str,
) -> Result<bool, SQLError> {
    if name == table.name {
        return Ok(false);
    }
    catalog.relation_name_available(&RelationIdentity::new(&table.schema, name).qualified_name())
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

pub fn allocate_default_index_name(
    catalog: &dyn IndexNameCatalog,
    table: &RelationIdentity,
    columns: &[crate::ast::IndexKey],
) -> Result<String, SQLError> {
    fn component(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut previous_was_separator = false;
        for ch in raw.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                out.extend(ch.to_lowercase());
                previous_was_separator = false;
            } else if !previous_was_separator && !out.is_empty() {
                out.push('_');
                previous_was_separator = true;
            }
        }
        while out.ends_with('_') {
            out.pop();
        }
        out
    }

    let mut parts = std::iter::once(component(&table.name))
        .chain(
            super::keys::key_names(columns)
                .iter()
                .map(|column| component(column)),
        )
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("index".to_string());
    }
    let base = format!("{}_idx", parts.join("_"));
    let available = |name: &str| -> Result<bool, SQLError> {
        let candidate = RelationIdentity::new(&table.schema, name).qualified_name();
        catalog.relation_name_available(&candidate)
    };
    if available(&base)? {
        return Ok(base);
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}{suffix}");
        if available(&candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!("u64 index-name suffix space is non-empty")
}
