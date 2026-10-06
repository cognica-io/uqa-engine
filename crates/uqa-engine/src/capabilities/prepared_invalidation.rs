//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend the session's prepared registry and current transaction log to catalog invalidation scheduling.

use super::MutationCoordinator;
use uqa_execution::statement::prepared::invalidation::PreparedCatalogChange;

impl MutationCoordinator<'_> {
    pub(crate) fn note_prepared_catalog_change(&self, change: PreparedCatalogChange) {
        if let Some(frame) = self.session.transactions.lock().last_mut() {
            frame.prepared_changes.record(change);
        }
        change.invalidate_with_routines(
            self.session.prepared.write().values_mut(),
            &self.session.routine_bodies,
        );
    }
    pub(crate) fn note_prepared_relation_change(&self, relation: &uqa_core::RelationIdentity) {
        let oid =
            self.storage
                .tables
                .read()
                .get(relation)
                .map(|table| table.relation_oids().relation)
                .or_else(|| {
                    self.durable
                        .views
                        .read()
                        .get(relation)
                        .map(|view| view.relation_oids().relation)
                })
                .or_else(|| {
                    self.durable
                        .foreign_tables
                        .read()
                        .get(relation)
                        .map(|table| table.relation_oids().relation)
                })
                .or_else(|| {
                    self.durable.sequence_object_ids.read().get(relation).and_then(|identity| {
                u32::try_from(uqa_execution::catalog::sequence::catalog_oids::sequence_catalog_oid(
                    &self.durable.sequence_catalog_oids.read(), identity,
                )).ok()
            })
                });
        if let Some(oid) = oid {
            self.note_prepared_catalog_change(PreparedCatalogChange::Relation(oid));
        }
    }
    pub(crate) fn note_prepared_table_change(&self, table: &crate::TableState) {
        self.note_prepared_catalog_change(PreparedCatalogChange::Relation(
            table.relation_oids().relation,
        ));
    }
}
