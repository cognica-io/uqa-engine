//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routine execution and dependency compilation in its creation namespace.

use uqa_sql::ast::{CreateFunction, FunctionBody};

use super::super::declaration::{
    compile_persisted_function_body, compile_persisted_function_dependencies,
};
use super::{CompiledFunctionBody, Engine, SQLError};

impl Engine {
    pub(super) fn compile_persisted_sql_function(
        &self,
        def: &CreateFunction,
    ) -> Result<CompiledFunctionBody, SQLError> {
        self.compile_stored_function_with(def, compile_persisted_function_body)
    }

    pub(super) fn stored_merge_dependency_body(
        &self,
        def: &CreateFunction,
    ) -> Result<Option<CompiledFunctionBody>, SQLError> {
        let FunctionBody::Statements(statements) = &def.body else {
            return Ok(None);
        };
        let mut has_removed_target = false;
        for statement in statements {
            crate::events::visit_stored_statement_merges(&mut statement.clone(), &mut |merge| {
                has_removed_target |= !self.dropped_stored_merge_targets(merge).is_empty();
                Ok(())
            })?;
        }
        if has_removed_target {
            self.compile_stored_function_with(def, compile_persisted_function_dependencies)
                .map(Some)
        } else {
            Ok(None)
        }
    }

    fn compile_stored_function_with(
        &self,
        def: &CreateFunction,
        compile: fn(&Engine, &CreateFunction) -> Result<CompiledFunctionBody, SQLError>,
    ) -> Result<CompiledFunctionBody, SQLError> {
        if !matches!(def.body, FunctionBody::Statements(_)) || def.creation_search_path.is_empty() {
            return compile(self, def);
        }
        let previous = {
            let mut state = self.session.state.write();
            std::mem::replace(&mut state.search_path, def.creation_search_path.clone())
        };
        let compiled = compile(self, def);
        self.session.state.write().search_path = previous;
        compiled
    }
}
