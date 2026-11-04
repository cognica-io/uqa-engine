//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select surviving sequence incarnations and stage their nontransactional rollback effects.

use super::{
    NontransactionalSequenceValue, NontransactionalSequenceValues, SessionLastSequenceReference,
};
use crate::catalog::sequence::{registry::SequenceCatalog, values::context::SequenceSessionWrite};
use uqa_sql::{ast::RelationPersistence, SQLError};
use uqa_storage::{
    CatalogFacade, SequenceSetValueResult, StorageBackendError, StorageBackendResult,
};

pub struct PersistentSequenceValues(
    Vec<(String, [u8; 16], [u8; 16], NontransactionalSequenceValue)>,
);

impl PersistentSequenceValues {
    pub fn select(catalog: &SequenceCatalog, values: &NontransactionalSequenceValues) -> Self {
        let mut selected = catalog
            .iter()
            .filter_map(|(relation, record)| {
                if record.persistence == RelationPersistence::Temporary {
                    return None;
                }
                let value =
                    values.position(record.object_id, record.state.definition_generation)?;
                (!value.autonomous).then(|| {
                    (
                        relation.qualified_name(),
                        record.object_id,
                        record.state.definition_generation,
                        *value,
                    )
                })
            })
            .collect::<Vec<_>>();
        selected.sort_by_key(|(_, object, _, _)| *object);
        Self(selected)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn persist(&self, catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
        for (name, object, definition, value) in &self.0 {
            match catalog.set_sequence_value(name, *object, *definition, value.current, value.called, value.log_count)? {
                SequenceSetValueResult::Set(_) => {}
                SequenceSetValueResult::Missing => return Err(StorageBackendError::Other(format!("sequence `{name}` disappeared while restoring its nontransactional value"))),
                SequenceSetValueResult::DefinitionChanged => return Err(StorageBackendError::Other(format!("sequence `{name}` definition changed while restoring its nontransactional value"))),
            }
        }
        Ok(())
    }
}

/// Preserve the selected catalog and session until every matching position has a complete admitted successor. The transaction owner handles a preparation failure through its ordinary rollback-cleanup outcome.
pub fn restore_values(
    catalog: &mut SequenceCatalog,
    values: &NontransactionalSequenceValues,
    session: &mut dyn SequenceSessionWrite,
) -> Result<(), SQLError> {
    let mut next = catalog.clone();
    for (relation, record) in catalog.iter() {
        if let Some(value) = values.position(record.object_id, record.state.definition_generation) {
            next.prepare_position(
                relation,
                record.object_id,
                record.state.definition_generation,
            )?
            .expect("selected recovery definition remains owned")
            .publish(uqa_storage::SequenceValuePosition {
                current: value.current,
                called: value.called,
                log_count: value.log_count,
            });
        }
    }
    let last = values.lastval().map(|object_id| {
        catalog
            .object_ids()
            .find(|(_, object)| **object == object_id)
            .map(|(relation, _)| SessionLastSequenceReference {
                relation: relation.clone(),
                object_id,
            })
    });
    let observations = values
        .observations()
        .filter_map(|(object_id, currval)| {
            catalog
                .object_ids()
                .find(|(_, object)| *object == object_id)
                .map(|(relation, _)| (relation.clone(), *object_id, *currval))
        })
        .collect::<Vec<_>>();
    *catalog = next;
    if let Some(last) = last {
        *session.last_mut() = last;
    }
    for (relation, object_id, currval) in observations {
        session
            .currvals_mut()
            .retain(|_, current| current.object_id != object_id);
        if let Some(currval) = currval {
            session.currvals_mut().insert(relation, currval);
        }
    }
    Ok(())
}
