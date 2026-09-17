//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project cache generations from the same committed/private view as native catalog data.

use std::collections::BTreeMap;

use rusqlite::types::ValueRef;
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{mvcc::VersionError, CatalogCacheRevisions};

use super::{text, Family, NativeRecordOwner, NativeSnapshot, Result, SQLiteError};
use crate::catalog::cache_revisions::{metadata_scope, revision_slot};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};

// Durable SQLite generations are nonnegative i64 values. Private identities occupy a disjoint domain and never enter physical rows or conflict preconditions.
const PRIVATE: u64 = 1 << 63;
type Object = ([u8; 16], [u8; 16]);

#[derive(Default)]
struct ObjectChange {
    data: u64,
    statistics: u64,
}

impl NativeSnapshot {
    pub(in crate::catalog) fn cache_revisions(&self) -> Result<CatalogCacheRevisions> {
        let mut revisions = RevisionProjection::new(&self.control);
        let owner = NativeRecordOwner::Database(self.database);
        // The native mapping fixes its physical schema; catalog schema changes are versioned data. Raw physical DDL is outside this bound session's contract.
        revisions.values.storage_schema = self
            .read_row(Family::Metadata, owner, &[text("schema_version")], |row| {
                row[1]
                    .as_str()
                    .ok()
                    .and_then(|version| version.parse().ok())
                    .ok_or_else(|| invalid("invalid native catalog format version"))
            })?
            .ok_or_else(|| invalid("missing native catalog format version"))?;
        self.visit_rows(Family::CacheRevisions, Some(owner), &[], |row| {
            let generation = row[2]
                .as_i64()
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| invalid("invalid native cache generation"))?;
            revisions.mark(string(row[0])?, string(row[1])?, generation)?;
            Ok(())
        })?;
        self.project_private_revisions(&mut revisions)?;
        Ok(revisions.values)
    }

    fn project_private_revisions(&self, revisions: &mut RevisionProjection<'_>) -> Result<()> {
        let mut objects: BTreeMap<Object, ObjectChange> = BTreeMap::new();
        let mut retained = BudgetedVec::<MemoryReservation>::new(self.control.memory());
        let mut after = BudgetedVec::new(self.control.memory());
        loop {
            let page = self.view.private_keys(
                &[],
                (!after.is_empty()).then_some(&*after),
                64,
                &self.control,
            )?;
            let Some(last) = page.last() else { break };
            after.clear();
            after.extend_from_slice(last.key())?;
            for change in page.iter() {
                let generation = PRIVATE
                    .checked_add(change.revision().as_u64())
                    .ok_or(VersionError::PrivateRevisionExhausted)?;
                let identity = NativeRecordIdentity::decode(change.key())?;
                let family = identity.family();
                match family {
                    Family::CacheRevisions => {
                        return Err(invalid("cache generations are owned by native publication"));
                    }
                    family if family.is_standalone_graph() => {}
                    Family::TableOwners
                    | Family::OccurrenceGuards
                    | Family::VectorGuards
                    | Family::GraphLookups
                    | Family::GraphPathPairs
                    | Family::GraphPathIndexState => {}
                    Family::Tables => revisions.mark("catalog", "", generation)?,
                    Family::GraphVertices | Family::GraphEdges => {
                        self.entity_revisions(revisions, change.key(), family, generation)?;
                    }
                    Family::Metadata | Family::NamedGraphs | Family::GraphMembership => {
                        self.named_revision(revisions, change.key(), family, generation)?;
                    }
                    Family::BtreeIndexes | Family::TableFieldAnalyzers => {
                        revisions.mark("registry", "", generation)?;
                    }
                    _ if family.layout().columns.contains(&"table_name") => {
                        let NativeRecordOwner::Object {
                            identity,
                            generation: incarnation,
                        } = identity.owner()
                        else {
                            return Err(invalid("table cache change has no object owner"));
                        };
                        let object = (identity, incarnation);
                        if !objects.contains_key(&object) {
                            retained.push(
                                self.control.memory().reserve(std::mem::size_of::<(
                                    Object,
                                    ObjectChange,
                                )>(
                                ))?,
                            )?;
                        }
                        let change = objects.entry(object).or_default();
                        let slot = if family == Family::ColumnStats {
                            &mut change.statistics
                        } else {
                            &mut change.data
                        };
                        *slot = (*slot).max(generation);
                    }
                    _ => revisions.mark("registry", "", generation)?,
                }
            }
        }
        self.project_object_revisions(revisions, &objects)
    }

    fn project_object_revisions(
        &self,
        revisions: &mut RevisionProjection<'_>,
        objects: &BTreeMap<Object, ObjectChange>,
    ) -> Result<()> {
        if objects.is_empty() {
            return Ok(());
        }
        // Visit the small owner directory, not table rows or payloads. Both names matter when a private rename/drop changes the current binding.
        let mut project = |row: &[ValueRef<'_>]| -> Result<()> {
            let array = |value: ValueRef<'_>| -> Result<[u8; 16]> {
                value
                    .as_blob()
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(|| invalid("invalid native table cache owner"))
            };
            if let Some(change) = objects.get(&(array(row[1])?, array(row[2])?)) {
                let name = string(row[0])?;
                if change.data != 0 {
                    revisions.mark("data", name, change.data)?;
                }
                if change.statistics != 0 {
                    revisions.mark("statistics", name, change.statistics)?;
                }
            }
            Ok(())
        };
        let owner = NativeRecordOwner::Database(self.database);
        self.visit_rows(Family::TableOwners, Some(owner), &[], &mut project)?;
        let prefix = NativeRecordIdentity::new(Family::TableOwners, owner)?
            .encode_prefix(&[], &self.control)?;
        self.view.committed().visit_prefix(
            &prefix,
            None,
            usize::MAX,
            &self.control,
            &mut |key, record| {
                if let Some(bytes) = record.value {
                    let (_, row) = decode_record(key, bytes, &self.control)?;
                    project(&row).map_err(|error| VersionError::Storage(error.into()))?;
                }
                Ok(true)
            },
        )?;
        Ok(())
    }

    fn named_revision(
        &self,
        revisions: &mut RevisionProjection<'_>,
        key: &[u8],
        family: Family,
        generation: u64,
    ) -> Result<()> {
        NativeRecordIdentity::visit_key_components(key, &self.control, |component, value| {
            if component
                == if family == Family::GraphMembership {
                    2
                } else {
                    0
                }
            {
                let name = string(value).map_err(|error| VersionError::Storage(error.into()))?;
                let (kind, name) = if family == Family::Metadata {
                    let Some(scope) = metadata_scope(name) else {
                        return Ok(());
                    };
                    scope
                } else {
                    ("graph", name)
                };
                revisions
                    .mark(kind, name, generation)
                    .map_err(|error| VersionError::Storage(error.into()))?;
            }
            Ok(())
        })?;
        Ok(())
    }

    fn entity_revisions(
        &self,
        revisions: &mut RevisionProjection<'_>,
        key: &[u8],
        family: Family,
        generation: u64,
    ) -> Result<()> {
        let mut id = 0;
        NativeRecordIdentity::visit_key_components(key, &self.control, |_, value| {
            id = value.as_i64().map_err(|_| {
                VersionError::InvalidEncoding("invalid native graph cache identity")
            })?;
            Ok(())
        })?;
        let kind = if family == Family::GraphVertices {
            "vertex"
        } else {
            "edge"
        };
        self.visit_paged_rows(
            Family::GraphMembership,
            &[text(kind), ValueRef::Integer(id)],
            |row| {
                revisions.mark("graph", string(row[2])?, generation)?;
                Ok(true)
            },
        )
    }
}

struct RevisionProjection<'a> {
    values: CatalogCacheRevisions,
    control: &'a StorageReadControl,
    retained: BudgetedVec<MemoryReservation>,
}

impl<'a> RevisionProjection<'a> {
    fn new(control: &'a StorageReadControl) -> Self {
        Self {
            values: CatalogCacheRevisions {
                graphs: Some(BTreeMap::new()),
                ..CatalogCacheRevisions::default()
            },
            control,
            retained: BudgetedVec::new(control.memory()),
        }
    }

    fn mark(&mut self, kind: &str, name: &str, generation: u64) -> Result<()> {
        let names = match kind {
            "graph" => self.values.graphs.as_ref(),
            "data" => Some(&self.values.table_data),
            "statistics" => Some(&self.values.column_statistics),
            "maintenance" => Some(&self.values.statistics_maintenance),
            _ => None,
        };
        if names.is_some_and(|names| !names.contains_key(name)) {
            let bytes = name
                .len()
                .checked_add(std::mem::size_of::<(String, u64)>())
                .ok_or(MemoryError::SizeOverflow)?;
            self.retained.push(self.control.memory().reserve(bytes)?)?;
        }
        let slot = revision_slot(&mut self.values, kind, name)?;
        *slot = (*slot).max(generation);
        Ok(())
    }
}

fn invalid(message: &'static str) -> SQLiteError {
    SQLiteError::StorageBackend(message.into())
}

fn string(value: ValueRef<'_>) -> Result<&str> {
    value
        .as_str()
        .map_err(|_| invalid("invalid native cache name"))
}
