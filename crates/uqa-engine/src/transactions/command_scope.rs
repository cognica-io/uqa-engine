//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-local mutation overlay ownership.

use crate::Engine;
use uqa_sql::SQLError;

pub(crate) fn run_mutation_command<R>(
    engine: &Engine,
    execute: impl FnOnce(&Engine) -> Result<R, SQLError>,
) -> Result<R, SQLError> {
    if engine.transaction_depth() == 0 {
        engine.transaction(execute)
    } else {
        execute(engine)
    }
}

pub(crate) use uqa_execution::mutation::command_scope::MutationOverlayScope;

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
