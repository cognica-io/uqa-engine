//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate native catalog references on the retained committed/private view before restoration.

#[cfg(test)]
mod tests;

use super::{Family, NativeRecordOwner, NativeSnapshot, Result, SQLiteError};
use crate::mvcc::native::NativeRecordIdentity;
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{catalog::RelationKind, mvcc::VersionError, read_control::StorageReadControl};

struct Definition {
    schema: BudgetedVec<u8>,
    name: BudgetedVec<u8>,
    kind: RelationKind,
}

impl Definition {
    fn read(
        row: &[ValueRef<'_>],
        kind: RelationKind,
        control: &StorageReadControl,
    ) -> Result<Self> {
        Ok(Self {
            schema: copy_text(row[0], control)?,
            name: copy_text(row[1], control)?,
            kind,
        })
    }
    fn name(&self) -> (&[u8], &[u8]) {
        (&self.schema, &self.name)
    }
}

const DEFINITIONS: [(Family, RelationKind); 5] = [
    (Family::Tables, RelationKind::Table),
    (Family::Views, RelationKind::View),
    (Family::Sequences, RelationKind::Sequence),
    (Family::ForeignTables, RelationKind::ForeignTable),
    (Family::CatalogIndexes, RelationKind::Index),
];

impl NativeSnapshot {
    pub(in crate::catalog) fn validate_catalog_namespace(&self) -> Result<()> {
        self.control.check()?;
        self.validate_definitions()?;
        self.validate_btree_parents()
    }

    fn validate_definitions(&self) -> Result<()> {
        let owner = NativeRecordOwner::Database(self.database);
        let mut schemas = BudgetedVec::new(self.control.memory());
        self.visit_rows(Family::Schemas, Some(owner), &[], |row| {
            schemas
                .push(copy_text(row[0], &self.control)?)
                .map_err(VersionError::from)?;
            Ok(())
        })?;
        schemas.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));

        let mut claims = BudgetedVec::new(self.control.memory());
        self.visit_rows(Family::Relations, Some(owner), &[], |row| {
            let kind = DEFINITIONS
                .iter()
                .find_map(|(_, kind)| {
                    (row[2] == ValueRef::Text(kind.as_str().as_bytes())).then_some(*kind)
                })
                .ok_or_else(|| invalid("native relation has an unknown kind"))?;
            claims
                .push(Definition::read(row, kind, &self.control)?)
                .map_err(VersionError::from)?;
            Ok(())
        })?;
        claims.sort_unstable_by(|a, b| a.name().cmp(&b.name()));
        for claim in claims.iter() {
            self.control.check()?;
            if schemas
                .binary_search_by(|schema| schema.as_ref().cmp(&claim.schema))
                .is_err()
            {
                return Err(invalid(
                    "native catalog relation references a missing schema",
                ));
            }
        }

        let mut definitions = BudgetedVec::new(self.control.memory());
        let mut index_tables = BudgetedVec::new(self.control.memory());
        for (family, kind) in DEFINITIONS {
            self.visit_rows(family, None, &[], |row| {
                if row[2] != ValueRef::Text(kind.as_str().as_bytes()) {
                    return Err(invalid(
                        "native definition kind disagrees with its record family",
                    ));
                }
                definitions
                    .push(Definition::read(row, kind, &self.control)?)
                    .map_err(VersionError::from)?;
                if family == Family::CatalogIndexes {
                    index_tables
                        .push(Definition::read(
                            &row[4..],
                            RelationKind::Table,
                            &self.control,
                        )?)
                        .map_err(VersionError::from)?;
                }
                Ok(())
            })?;
        }
        definitions.sort_unstable_by(|a, b| a.name().cmp(&b.name()));
        for pair in definitions.windows(2) {
            if pair[0].name() == pair[1].name() {
                return Err(invalid(
                    "multiple native definitions claim the same relation name",
                ));
            }
        }
        for definition in definitions.iter() {
            self.control.check()?;
            if !contains(&claims, definition) {
                return Err(invalid(
                    "native definition has no matching catalog relation",
                ));
            }
        }
        for claim in claims.iter() {
            self.control.check()?;
            if !contains(&definitions, claim) {
                return Err(invalid(
                    "native catalog relation has no matching definition",
                ));
            }
        }
        for table in index_tables.iter() {
            self.control.check()?;
            if !contains(&definitions, table) {
                return Err(invalid("native catalog index references a missing table"));
            }
        }
        Ok(())
    }

    /// Every live entry belongs to an index that exists. The entries of one index are contiguous in key order and share its record, so the first of them answers for all the others and the scan continues after the last key they can have: the work follows the number of indexes, not of their entries. Only the entries of an index that does not exist are all visited, because any of them may be live; none of them is retained.
    fn validate_btree_parents(&self) -> Result<()> {
        let control = &self.control;
        let prefix = NativeRecordIdentity::family_prefix(Family::BtreeIndexEntries, control)?;
        let mut after: Option<BudgetedVec<u8>> = None;
        loop {
            control.check()?;
            let mut first = None;
            self.view
                .visit_keys(&prefix, after.as_deref(), 1, control, &mut |key, _| {
                    let mut owned = BudgetedVec::new(control.memory());
                    owned.extend_from_slice(key)?;
                    first = Some(owned);
                    Ok(false)
                })?;
            let Some(key) = first else {
                return Ok(());
            };
            // The key visitor is released before the index record is read. Entry values and unrelated document/index payloads are never hydrated.
            let (index, entries) = btree_index(&key, control)?;
            if !self
                .view
                .metadata(&index, control)?
                .is_some_and(|record| record.live)
            {
                let mut live = false;
                self.view
                    .visit_keys(&entries, None, usize::MAX, control, &mut |_, metadata| {
                        live = metadata.live;
                        Ok(!live)
                    })?;
                if live {
                    return Err(invalid("native B-tree entry references a missing index"));
                }
            }
            let Some(end) = crate::read_control::prefix_upper_bound(&entries, control)? else {
                // No key follows a prefix that has no successor.
                return Ok(());
            };
            after = Some(end);
        }
    }
}

fn contains(definitions: &[Definition], expected: &Definition) -> bool {
    definitions
        .binary_search_by(|definition| definition.name().cmp(&expected.name()))
        .is_ok_and(|position| definitions[position].kind == expected.kind)
}

/// The record of the index that an entry belongs to, and the prefix that every entry of that index begins with.
fn btree_index(
    key: &[u8],
    control: &StorageReadControl,
) -> Result<(BudgetedVec<u8>, BudgetedVec<u8>)> {
    let mut field = BudgetedVec::new(control.memory());
    let mut binary = false;
    let identity = NativeRecordIdentity::visit_key_components(key, control, |position, value| {
        if position == 0 {
            let bytes = match value {
                ValueRef::Text(bytes) => bytes,
                ValueRef::Blob(bytes) => {
                    binary = true;
                    bytes
                }
                _ => return Err(VersionError::InvalidEncoding("invalid native B-tree field")),
            };
            field.extend_from_slice(bytes)?;
        }
        Ok(())
    })?;
    let field = if binary {
        ValueRef::Blob(&field)
    } else {
        ValueRef::Text(&field)
    };
    Ok((
        NativeRecordIdentity::new(Family::BtreeIndexes, identity.owner())?
            .encode_key(&[field], control)?,
        identity.encode_prefix(&[field], control)?,
    ))
}

fn copy_text(value: ValueRef<'_>, control: &StorageReadControl) -> Result<BudgetedVec<u8>> {
    control.check()?;
    let text = value
        .as_str()
        .map_err(|_| invalid("native namespace name is not text"))?;
    let mut copied = BudgetedVec::new(control.memory());
    copied
        .extend_from_slice(text.as_bytes())
        .map_err(VersionError::from)?;
    Ok(copied)
}

fn invalid(message: &str) -> SQLiteError {
    SQLiteError::StorageBackend(message.into())
}
