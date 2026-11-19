//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind native sequence removal to the transaction and concrete registry/cache publication boundary.
use crate::Engine;
use uqa_execution::schema::sequences::removal::{
    SequenceRemovalContext, SequenceRemovalInputs, SequenceRemovalPublication,
};
impl SequenceRemovalInputs for Engine {
    fn sequence_removal_context(&self) -> SequenceRemovalContext<'_> {
        Engine::sequence_removal_context(self)
    }
}
impl Engine {
    pub(crate) fn sequence_removal_context(&self) -> SequenceRemovalContext<'_> {
        SequenceRemovalContext {
            publication: self,
            privileges: self.sequence_privilege_inquiry(),
            dependencies: self.sequence_dependency_context(),
        }
    }
    pub fn drop_sequence(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_definition_transaction(|engine| {
            uqa_execution::schema::removal::direct::drop_sequence(
                &engine.relation_removal_context(),
                name,
            )
        })
        .map_err(|error| error.to_string())
    }
}
impl SequenceRemovalPublication for Engine {
    fn remove_state(&self, name: &str) -> Result<bool, uqa_sql::SQLError> {
        let removed = uqa_execution::catalog::sequence::registry::publication::remove(
            &mut self.durable.sequences.write(),
            self.storage.catalog.as_deref(),
            name,
        )?;
        if let Some(record) = removed {
            let object_id = record.object_id;
            let oid = record.catalog_oid.unwrap_or_else(|| {
                uqa_execution::catalog::projection::legacy_sequence_relation_oid(object_id) as u32
            });
            self.note_prepared_catalog_change(
                uqa_execution::statement::prepared::invalidation::PreparedCatalogChange::Relation(
                    oid,
                ),
            );
            let mut session = self.session.state.write();
            session
                .sequence_currvals
                .retain(|_, current| current.object_id != object_id);
            if session
                .last_sequence
                .as_ref()
                .is_some_and(|last| last.object_id == object_id)
            {
                session.last_sequence = None;
            }
            drop(session);
            self.session
                .sequence_caches
                .lock()
                .retain(|_, cache| cache.object_id != object_id);
            self.note_catalog_registry_changed();
        }
        Ok(removed.is_some())
    }
}
