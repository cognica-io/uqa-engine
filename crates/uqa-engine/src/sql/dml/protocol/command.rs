//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-local mutation overlay ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::engine_capabilities::MutationCoordinator;
use crate::{DocId, Document, Engine};
use uqa_sql::SQLError;

#[derive(Clone, Default)]
pub(crate) struct CommandMutationOverlay {
    pub(crate) documents: BTreeMap<String, BTreeMap<DocId, Option<Arc<Document>>>>,
    pub(crate) exact_indexes: BTreeMap<String, BTreeMap<Vec<String>, CommandExactIndex>>,
}

#[derive(Clone, Default)]
pub(crate) struct CommandExactIndex {
    pub(crate) doc_ids_by_key: BTreeMap<Vec<u8>, BTreeSet<DocId>>,
}

pub(in crate::sql) fn run_mutation_command<R>(
    engine: &Engine,
    execute: impl FnOnce(&Engine) -> Result<R, SQLError>,
) -> Result<R, SQLError> {
    if engine.transaction_depth() == 0 {
        engine.transaction(execute)
    } else {
        execute(engine)
    }
}

pub(in crate::sql) struct MutationOverlayScope<'a> {
    coordinator: MutationCoordinator<'a>,
}

impl<'a> MutationOverlayScope<'a> {
    pub(in crate::sql) fn new(engine: &'a Engine) -> Self {
        let coordinator = engine.mutation_coordinator();
        coordinator.begin_command_mutation_overlay();
        Self { coordinator }
    }
}

impl Drop for MutationOverlayScope<'_> {
    fn drop(&mut self) {
        self.coordinator.end_command_mutation_overlay();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_command_uses_exactly_one_transaction_frame() {
        let engine = Engine::new();
        let implicit_depth = run_mutation_command(&engine, |engine| {
            Ok::<_, SQLError>(engine.transaction_depth())
        })
        .unwrap();
        assert_eq!(implicit_depth, 1);
        assert_eq!(engine.transaction_depth(), 0);

        engine
            .transaction(|engine| {
                let nested_depth = run_mutation_command(engine, |engine| {
                    Ok::<_, SQLError>(engine.transaction_depth())
                })?;
                assert_eq!(nested_depth, 1);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn mutation_overlay_scope_cleans_up_on_drop() {
        let engine = Engine::new();
        assert!(!engine.command_mutation_overlay_active());
        {
            let _overlay = MutationOverlayScope::new(&engine);
            assert!(engine.command_mutation_overlay_active());
        }
        assert!(!engine.command_mutation_overlay_active());
    }
}
