//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Surviving routine body candidates and exact registry identities for source rewrites.

use super::RoutineRegistry;
use crate::{
    ast::{CreateFunction, FunctionBinding, FunctionBody},
    catalog::events::RuleColumnDependency,
    routines::{routine_signature_types, SQLUserFunction},
    SQLError,
};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::RelationIdentity;

pub fn routine_column_drop_dependencies(
    columns: BTreeSet<(String, String)>,
) -> Result<BTreeSet<RuleColumnDependency>, SQLError> {
    columns
        .into_iter()
        .map(|(table, column)| {
            Ok(RuleColumnDependency {
                relation: RelationIdentity::from_legacy_name(&table).map_err(SQLError::Internal)?,
                column,
            })
        })
        .collect()
}

pub fn rewritten_routine_definitions(
    registry: &RoutineRegistry,
    mut rewrite: impl FnMut(&mut crate::ast::Statement) -> Result<bool, SQLError>,
) -> Result<Vec<CreateFunction>, SQLError> {
    let mut definitions = Vec::new();
    for overloads in registry.values() {
        for function in overloads {
            let mut definition = function.def.clone();
            let mut changed = false;
            if let FunctionBody::Statements(statements) = &mut definition.body {
                for statement in statements {
                    changed |= rewrite(statement)?;
                }
            }
            if changed {
                definitions.push(definition);
            }
        }
    }
    Ok(definitions)
}

pub fn routine_column_alias_drop_candidates(
    columns: crate::binding::stored_columns::StoredColumnBindingContext<'_>,
    registry: &RoutineRegistry,
    dependencies: &BTreeSet<RuleColumnDependency>,
    removed_routines: &[FunctionBinding],
) -> Result<Vec<CreateFunction>, SQLError> {
    let mut definitions = Vec::new();
    for function in registry.values().flatten() {
        if removed_routines.iter().any(|target| {
            target.object_id == function.def.object_id
                && target.name == function.def.name
                && target.argument_types == routine_signature_types(&function.def)
        }) {
            continue;
        }
        let mut definition = function.def.clone();
        let FunctionBody::Statements(statements) = &mut definition.body else {
            continue;
        };
        let mut changed = false;
        for statement in statements {
            changed |=
                crate::binding::stored_columns::remove_stored_statement_source_column_aliases(
                    columns,
                    statement,
                    dependencies,
                )?;
        }
        if changed {
            definitions.push(definition);
        }
    }
    Ok(definitions)
}

pub fn routine_body_rewrite_target<'a>(
    rewritten: &'a mut RoutineRegistry,
    definition: &CreateFunction,
) -> Result<&'a mut Arc<SQLUserFunction>, SQLError> {
    let signature = routine_signature_types(definition);
    rewritten
        .get_mut(&definition.name)
        .and_then(|overloads| {
            overloads.iter_mut().find(|function| {
                function.def.object_id == definition.object_id
                    && function.def.is_procedure == definition.is_procedure
                    && routine_signature_types(&function.def) == signature
            })
        })
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "stored routine {} disappeared before its body rewrite",
                definition.name
            ))
        })
}
