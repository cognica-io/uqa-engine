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
        self.native_record_revisions_with_control(
            records,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )?
        .map(|revisions| {
            revisions.into_uncontrolled().map_err(|_| {
                crate::SQLiteError::StorageBackend("ordinary catalog revision owner".into())
            })
        })
        .transpose()
    }

    pub(in crate::catalog) fn native_record_revisions_with_control(
        &self,
        records: &[CatalogRecordRef<'_>],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<Option<CatalogRecordRevision>>>>> {
        control.check_cancellation()?;
        self.read_native(|snapshot| {
            let mut output = uqa_core::memory::ProductionVec::new(*control);
            output
                .reserve(records.len())
                .map_err(uqa_storage::StorageBackendError::from)?;
            for record in records {
                control.check_cancellation()?;
                let value = snapshot.catalog_record_revision(*record, control)?;
                let value = control
                    .finish(value, control.empty_reservation())
                    .map_err(uqa_storage::StorageBackendError::from)?;
                output
                    .push_produced(value)
                    .map_err(uqa_storage::StorageBackendError::from)?;
            }
            Ok(output
                .finish()
                .map_err(uqa_storage::StorageBackendError::from)?)
        })
    }
}

impl NativeSnapshot {
    fn catalog_record_revision(
        &self,
        record: CatalogRecordRef<'_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<CatalogRecordRevision>> {
        self.control.check()?;
        let database = NativeRecordOwner::Database(self.database);
        let key = match record {
            CatalogRecordRef::Metadata(name) => {
                NativeRecordIdentity::new(Family::Metadata, database)?
                    .encode_key(&[text(name)], &self.control)?
            }
            CatalogRecordRef::Relation(RelationKind::Table, relation) => {
                let name = relation
                    .qualified_name_with_control(control)
                    .map_err(uqa_storage::StorageBackendError::from)?;
                let Some((owner, true)) = self.table_binding(&name)? else {
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
