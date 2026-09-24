//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint indexes occupy the same relation namespace as explicit indexes.

#[cfg(test)]
mod tests;

use crate::{
    ast::{TableKeyConstraint, TableKeyConstraintKind},
    SQLError,
};
use uqa_core::RelationIdentity;

/// Relation-namespace visibility and existing constraint identities, without index mutation.
pub trait IndexNameCatalog {
    fn existing_constraint_keys(&self, table: &str) -> Result<Vec<TableKeyConstraint>, SQLError>;
    fn existing_constraint_names(
        &self,
        table: &str,
    ) -> Result<std::collections::BTreeSet<String>, SQLError>;
    fn automatic_constraint_names(
        &self,
        table: &str,
    ) -> Result<std::collections::BTreeSet<String>, SQLError>;
    fn relation_name_available(&self, qualified_name: &str) -> Result<bool, SQLError>;
}

pub fn name_constraint_indexes(
    catalog: &dyn IndexNameCatalog,
    table: &str,
    keys: &mut [TableKeyConstraint],
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let existing = catalog.existing_constraint_keys(table)?;
    let occupied = catalog.existing_constraint_names(table)?;
    let automatic = catalog.automatic_constraint_names(table)?;
    let mut used = std::collections::BTreeSet::new();
    for key in keys {
        if let Some(old) = existing.iter().find(|old| {
            *old == key
                || key.catalog_identity.is_some_and(|identity| {
                    old.catalog_identity
                        .is_some_and(|old| old.object_id == identity.object_id)
                })
        }) {
            key.name.clone_from(&old.name);
            used.extend(key.name.iter().cloned());
            continue;
        }
        if let Some(name) = &key.name {
            if occupied.contains(name) {
                return Err(crate::schema::constraint_changes::constraint_error(
                    "42710",
                    format!(
                        "constraint \"{name}\" for relation \"{}\" already exists",
                        relation.name
                    ),
                ));
            }
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
            if !used.contains(&candidate)
                && !occupied.contains(&candidate)
                && !automatic.contains(&candidate)
                && available(catalog, &relation, &candidate)?
            {
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
pub(in crate::schema) fn object_name(table: &str, columns: &str, label: &str) -> String {
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
    let component = super::keys::key_names(columns).join("_");
    for number in 0_u64.. {
        let label = if number == 0 {
            "idx".to_owned()
        } else {
            format!("idx{number}")
        };
        let candidate = object_name(&table.name, &component, &label);
        if available(catalog, table, &candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!("u64 index-name suffix space is non-empty")
}
