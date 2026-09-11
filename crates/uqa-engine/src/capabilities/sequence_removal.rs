//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind native sequence removal to the transaction and concrete registry/cache publication boundary.
use crate::{Engine, StorageBackendResult};
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
            names: self,
            publication: self,
            privileges: self.sequence_privilege_inquiry(),
            dependencies: self.sequence_dependency_context(),
            routines: self.routine_removal_context(),
            views: self.view_removal_context(),
            events: self.event_lifecycle_context(),
        }
    }
    pub fn drop_sequence(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            engine.sequence_removal_context().drop_sequence(name)
        })
    }
    pub(crate) fn drop_owned_sequence(
        &self,
        name: &str,
        cascade: bool,
    ) -> StorageBackendResult<()> {
        self.sequence_removal_context()
            .drop_owned_sequence(name, cascade)
    }
}
impl SequenceRemovalPublication for Engine {
    fn remove_state(&self, name: &str) -> Result<bool, String> {
        let relation = Self::resolved_relation_identity(name)
            .map_err(|err| format!("resolve sequence `{name}`: {err}"))?;
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| format!("Sequence `{name}` has no object identity"))?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        let removed = if temporary {
            self.durable.sequences.read().contains_key(&relation)
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_sequence_row(name)
                .map_err(|err| format!("persist sequence catalog: {err}"))?
        } else {
            self.durable.sequences.read().contains_key(&relation)
        };
        if removed {
            self.durable.sequences.write().remove(&relation);
            self.durable.sequence_object_ids.write().remove(&relation);
            self.durable.sequence_persistence.write().remove(&relation);
            self.durable.sequence_security.write().remove(&relation);
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
        Ok(removed)
    }
}

#[cfg(test)]
mod tests;
