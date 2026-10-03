//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The foreign keys a partition holds for the foreign keys of its partitioned parent: an equivalent foreign key of its own that it attaches, or a copy, as `PostgreSQL`'s `tryAttachPartitionForeignKey` and `addFkConstraint` decide.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{ColumnDef, ForeignKey, TableConstraintSet};
use crate::SQLError;

#[cfg(test)]
mod tests;

fn local_name(table: &str) -> Result<String, SQLError> {
    uqa_core::RelationIdentity::from_legacy_name(table)
        .map(|relation| relation.name)
        .map_err(SQLError::Internal)
}

/// The foreign key of `partition` that `parent_key` attaches among `candidates`, the partition's foreign keys that copy none of its parent's, in name order: the first that references the same key through the same columns with the same deferrability, actions and match type. A candidate that differs from `parent_key` in enforceability alone is an error, as `PostgreSQL` refuses to attach it or to add a second constraint beside it.
pub fn attachable_foreign_key<'a>(
    partition: &str,
    parent_key: &ForeignKey,
    candidates: &'a [ForeignKey],
) -> Result<Option<&'a ForeignKey>, SQLError> {
    let mut ordered = candidates.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.name.cmp(&right.name));
    for candidate in ordered {
        if candidate.ref_table != parent_key.ref_table
            || candidate.local_columns != parent_key.local_columns
            || candidate.ref_columns != parent_key.ref_columns
            || candidate.period != parent_key.period
        {
            continue;
        }
        if candidate.enforced != parent_key.enforced {
            return Err(SQLError::Diagnostic {
                sqlstate: "42P16".into(),
                message: format!(
                    "constraint \"{}\" enforceability conflicts with constraint \"{}\" on relation \"{}\"",
                    parent_key.name.as_deref().unwrap_or_default(),
                    candidate.name.as_deref().unwrap_or_default(),
                    local_name(partition)?
                ),
                detail: None,
                hint: None,
            });
        }
        if candidate.deferrable == parent_key.deferrable
            && candidate.initially_deferred == parent_key.initially_deferred
            && candidate.on_update == parent_key.on_update
            && candidate.on_delete == parent_key.on_delete
            && candidate.match_type == parent_key.match_type
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// A partition's copy of `parent_key`, which keeps the parent's name unless one of the partition's constraints in `used` holds it, when it takes the parent's name with the first numeric suffix that no constraint of the partition's schema, in `schema`, holds, as `PostgreSQL`'s `addFkConstraint` names it.
pub fn partition_foreign_key_copy(
    parent_key: &ForeignKey,
    used: &BTreeSet<String>,
    schema: &mut BTreeSet<String>,
) -> Result<ForeignKey, SQLError> {
    let mut copy = parent_key.clone();
    copy.catalog_identity = None;
    // Only the foreign key without a parent derives constraints on referenced partitions.
    copy.referenced_partitions.clear();
    if let Some(name) = copy.name.as_ref().filter(|name| used.contains(*name)) {
        copy.name = Some(
            crate::schema::constraint_metadata::choose_suffixed_constraint_name(name, schema)
                .map_err(|error| SQLError::Internal(error.to_string()))?,
        );
    }
    Ok(copy)
}

/// The object identities of the foreign keys a declaration holds, which its partitions' copies share.
pub fn declared_foreign_key_families(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
) -> BTreeSet<[u8; 16]> {
    columns
        .iter()
        .filter_map(|column| column.references.as_ref()?.object_id)
        .chain(
            constraints
                .foreign_keys
                .iter()
                .filter_map(|foreign_key| foreign_key.object_id),
        )
        .collect()
}

/// A foreign key of a declaration, by the catalog row it publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaredForeignKey {
    Column(usize),
    Table(usize),
}

impl DeclaredForeignKey {
    /// The foreign key of the declaration whose catalog row is `identity`.
    pub fn by_catalog_identity(
        columns: &[ColumnDef],
        constraints: &TableConstraintSet,
        identity: crate::ast::ConstraintCatalogIdentity,
    ) -> Option<Self> {
        columns
            .iter()
            .position(|column| {
                column
                    .references
                    .as_ref()
                    .is_some_and(|reference| reference.catalog_identity == Some(identity))
            })
            .map(Self::Column)
            .or_else(|| {
                constraints
                    .foreign_keys
                    .iter()
                    .position(|foreign_key| foreign_key.catalog_identity == Some(identity))
                    .map(Self::Table)
            })
    }

    /// The foreign key of the declaration that belongs to the family `object_id`.
    pub fn by_family(
        columns: &[ColumnDef],
        constraints: &TableConstraintSet,
        object_id: [u8; 16],
    ) -> Option<Self> {
        columns
            .iter()
            .position(|column| {
                column
                    .references
                    .as_ref()
                    .is_some_and(|reference| reference.object_id == Some(object_id))
            })
            .map(Self::Column)
            .or_else(|| {
                constraints
                    .foreign_keys
                    .iter()
                    .position(|foreign_key| foreign_key.object_id == Some(object_id))
                    .map(Self::Table)
            })
    }

    /// The foreign key with its stored targets, as a table-level declaration.
    pub fn foreign_key(
        self,
        columns: &[ColumnDef],
        constraints: &TableConstraintSet,
    ) -> Option<ForeignKey> {
        match self {
            Self::Column(index) => {
                let column = columns.get(index)?;
                column.references.as_ref().map(|reference| {
                    crate::schema::foreign_keys::column_foreign_key(column, reference)
                })
            }
            Self::Table(index) => constraints.foreign_keys.get(index).cloned(),
        }
    }

    pub fn validated(self, columns: &[ColumnDef], constraints: &TableConstraintSet) -> bool {
        match self {
            Self::Column(index) => columns[index]
                .references
                .as_ref()
                .is_some_and(|reference| reference.validated),
            Self::Table(index) => constraints.foreign_keys[index].validated,
        }
    }

    pub fn set_validated(
        self,
        columns: &mut [ColumnDef],
        constraints: &mut TableConstraintSet,
        validated: bool,
    ) {
        match self {
            Self::Column(index) => {
                if let Some(reference) = columns[index].references.as_mut() {
                    reference.validated = validated;
                }
            }
            Self::Table(index) => constraints.foreign_keys[index].validated = validated,
        }
    }

    /// Apply an `ALTER CONSTRAINT` enforceability and deferrability change. A foreign key that stops being enforced is no longer valid, and one that becomes enforced is not valid until its rows are validated; reports whether it became enforced.
    pub fn alter(
        self,
        columns: &mut [ColumnDef],
        constraints: &mut TableConstraintSet,
        enforceability: Option<bool>,
        deferrability: Option<(bool, bool)>,
    ) -> bool {
        let (enforced, validated, deferrable, initially_deferred) = match self {
            Self::Column(index) => {
                let Some(reference) = columns[index].references.as_mut() else {
                    return false;
                };
                (
                    &mut reference.enforced,
                    &mut reference.validated,
                    &mut reference.deferrable,
                    &mut reference.initially_deferred,
                )
            }
            Self::Table(index) => {
                let foreign_key = &mut constraints.foreign_keys[index];
                (
                    &mut foreign_key.enforced,
                    &mut foreign_key.validated,
                    &mut foreign_key.deferrable,
                    &mut foreign_key.initially_deferred,
                )
            }
        };
        let mut became_enforced = false;
        match enforceability {
            Some(false) => {
                *enforced = false;
                *validated = false;
            }
            Some(true) if !*enforced => {
                *enforced = true;
                *validated = false;
                became_enforced = true;
            }
            Some(true) | None => {}
        }
        if let Some((deferrable_value, initially_deferred_value)) = deferrability {
            *deferrable = deferrable_value;
            *initially_deferred = initially_deferred_value;
        }
        became_enforced
    }

    pub fn set_family(
        self,
        columns: &mut [ColumnDef],
        constraints: &mut TableConstraintSet,
        object_id: [u8; 16],
    ) {
        match self {
            Self::Column(index) => {
                if let Some(reference) = columns[index].references.as_mut() {
                    reference.object_id = Some(object_id);
                }
            }
            Self::Table(index) => constraints.foreign_keys[index].object_id = Some(object_id),
        }
    }
}

/// Move the foreign keys of a declaration whose families joined others into those families: the copies of a foreign key that a partition attached to its parent's follow it. Reports whether any moved.
pub fn rejoin_foreign_key_families(
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    joined: &BTreeMap<[u8; 16], [u8; 16]>,
) -> bool {
    let mut changed = false;
    for object_id in columns
        .iter_mut()
        .filter_map(|column| column.references.as_mut())
        .map(|reference| &mut reference.object_id)
        .chain(
            constraints
                .foreign_keys
                .iter_mut()
                .map(|foreign_key| &mut foreign_key.object_id),
        )
    {
        if let Some(target) = object_id.and_then(|current| joined.get(&current)) {
            *object_id = Some(*target);
            changed = true;
        }
    }
    changed
}
