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
    let mut namer = ConstraintIndexNamer::new(catalog, table)?;
    for key in keys {
        namer.name(key)?;
    }
    Ok(())
}

/// Names the indexes of a relation's keys one at a time, each after the keys named before it, as `index_create` names each new index.
pub struct ConstraintIndexNamer<'a> {
    catalog: &'a dyn IndexNameCatalog,
    relation: RelationIdentity,
    existing: Vec<TableKeyConstraint>,
    occupied: std::collections::BTreeSet<String>,
    automatic: std::collections::BTreeSet<String>,
    used: std::collections::BTreeSet<String>,
}

impl<'a> ConstraintIndexNamer<'a> {
    pub fn new(catalog: &'a dyn IndexNameCatalog, table: &str) -> Result<Self, SQLError> {
        Ok(Self {
            catalog,
            relation: RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?,
            existing: catalog.existing_constraint_keys(table)?,
            occupied: catalog.existing_constraint_names(table)?,
            automatic: catalog.automatic_constraint_names(table)?,
            used: std::collections::BTreeSet::new(),
        })
    }

    /// Constraints the statement creates on the relation before its keys, such as a new table's CHECK and NOT NULL constraints.
    pub fn occupy(&mut self, names: impl IntoIterator<Item = String>) {
        self.occupied.extend(names);
    }

    /// Name a key's index. A retained key keeps its name; an explicit name must not belong to a relation, then to another constraint of the relation; any other key takes the first free generated name.
    pub fn name(&mut self, key: &mut TableKeyConstraint) -> Result<(), SQLError> {
        if let Some(old) = self.existing.iter().find(|old| {
            *old == key
                || key.catalog_identity.is_some_and(|identity| {
                    old.catalog_identity
                        .is_some_and(|old| old.object_id == identity.object_id)
                })
        }) {
            key.name.clone_from(&old.name);
            self.used.extend(key.name.iter().cloned());
            return Ok(());
        }
        if let Some(name) = &key.name {
            if self.used.contains(name) || !available(self.catalog, &self.relation, name)? {
                return Err(SQLError::Routine {
                    sqlstate: "42P07".into(),
                    message: format!("relation \"{name}\" already exists"),
                });
            }
            if self.occupied.contains(name) {
                return Err(crate::schema::constraint_changes::constraint_error(
                    "42710",
                    format!(
                        "constraint \"{name}\" for relation \"{}\" already exists",
                        self.relation.name
                    ),
                ));
            }
            self.used.insert(name.clone());
            return Ok(());
        }
        let suffix = if key.kind == TableKeyConstraintKind::PrimaryKey {
            "pkey"
        } else {
            "key"
        };
        let component = if key.kind == TableKeyConstraintKind::PrimaryKey {
            String::new()
        } else {
            super::keys::key_names(&constraint_index_attributes(key)).join("_")
        };
        for number in 0_u64.. {
            let label = if number == 0 {
                suffix.into()
            } else {
                format!("{suffix}{number}")
            };
            let candidate = object_name(&self.relation.name, &component, &label);
            if !self.used.contains(&candidate)
                && !self.occupied.contains(&candidate)
                && !self.automatic.contains(&candidate)
                && available(self.catalog, &self.relation, &candidate)?
            {
                self.used.insert(candidate.clone());
                key.name = Some(candidate);
                break;
            }
        }
        Ok(())
    }
}

/// The attributes of a constraint's supporting index: its key columns followed by the columns it includes.
pub fn constraint_index_attributes(key: &TableKeyConstraint) -> Vec<crate::ast::IndexKey> {
    key.columns
        .iter()
        .chain(&key.included_columns)
        .cloned()
        .map(crate::ast::IndexKey::Column)
        .collect()
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
