//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read exact native catalog record revisions through the owning session snapshot.

use super::{text, Catalog, Family, NativeRecordOwner, NativeSnapshot, Result};
use crate::mvcc::native::NativeRecordIdentity;
use uqa_storage::catalog::{CatalogRecordRef, CatalogRecordRevision, RelationKind};

impl Catalog {
    pub(in crate::catalog) fn native_record_revisions(
        &self,
        records: &[CatalogRecordRef<'_>],
    ) -> Result<Option<Vec<Option<CatalogRecordRevision>>>> {
        self.read_native(|snapshot| {
            records
                .iter()
                .map(|record| snapshot.catalog_record_revision(*record))
                .collect()
        })
    }
}

impl NativeSnapshot {
    fn catalog_record_revision(
        &self,
        record: CatalogRecordRef<'_>,
    ) -> Result<Option<CatalogRecordRevision>> {
        self.control.check()?;
        let database = NativeRecordOwner::Database(self.database);
        let key = match record {
            CatalogRecordRef::Metadata(name) => {
                NativeRecordIdentity::new(Family::Metadata, database)?
                    .encode_key(&[text(name)], &self.control)?
            }
            CatalogRecordRef::Relation(RelationKind::Table, relation) => {
                let Some((owner, true)) = self.table_binding(&relation.qualified_name())? else {
                    return Ok(None);
                };
                NativeRecordIdentity::new(Family::Tables, owner)?.encode_key(&[], &self.control)?
            }
            CatalogRecordRef::Relation(RelationKind::Sequence, relation) => {
                let Some(owner) = self.sequence_named(relation)? else {
                    return Ok(None);
                };
                NativeRecordIdentity::new(Family::Sequences, owner)?
                    .encode_key(&[], &self.control)?
            }
            CatalogRecordRef::Relation(kind, relation) => {
                let family = match kind {
                    RelationKind::View => Family::Views,
                    RelationKind::ForeignTable => Family::ForeignTables,
                    RelationKind::Index => Family::CatalogIndexes,
                    RelationKind::Table | RelationKind::Sequence => unreachable!(),
                };
                NativeRecordIdentity::new(family, database)?.encode_key(
                    &[text(&relation.schema), text(&relation.name)],
                    &self.control,
                )?
            }
        };
        Ok(self
            .view
            .record_revision(self.history, &key, &self.control)?
            .map(CatalogRecordRevision::Record))
    }
}
