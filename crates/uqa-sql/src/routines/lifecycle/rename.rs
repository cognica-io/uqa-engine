//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine rename identities, collision checks, registry movement, and stored AST rewrites.

use super::RoutineRegistry;
use crate::{
    ast::{CreateFunction, FunctionBinding, FunctionBody, RenameRoutineStmt},
    routines::{routine_signature_types, SQLUserFunction},
    SQLError,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;

pub struct RoutineRenameTarget {
    pub old_name: String,
    pub new_name: String,
    pub position: usize,
    pub binding: FunctionBinding,
}

pub fn routine_rename_identity(name: &str) -> Result<RelationIdentity, SQLError> {
    RelationIdentity::from_legacy_name(name)
        .map_err(|error| SQLError::Internal(format!("decode routine identity `{name}`: {error}")))
}

pub fn finish_routine_rename_target(
    stmt: &RenameRoutineStmt,
    old_name: String,
    position: usize,
    definition: &CreateFunction,
    old_identity: &RelationIdentity,
    registry: &RoutineRegistry,
) -> Result<RoutineRenameTarget, SQLError> {
    let signature = routine_signature_types(definition);
    let new_name = routine_rename_destination(stmt, old_identity, &signature, registry)?;
    let object_id = definition.object_id.ok_or_else(|| {
        SQLError::Internal(format!(
            "routine `{old_name}` has no catalog object identity"
        ))
    })?;
    Ok(RoutineRenameTarget {
        old_name: old_name.clone(),
        new_name,
        position,
        binding: FunctionBinding {
            object_id: Some(object_id),
            name: old_name,
            argument_types: signature,
            builtin: false,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        },
    })
}

fn routine_rename_destination(
    stmt: &RenameRoutineStmt,
    old_identity: &RelationIdentity,
    signature: &[String],
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
) -> Result<String, SQLError> {
    let (new_schema, new_local_name) =
        RelationIdentity::parse_reference(&stmt.new_name).map_err(|error| SQLError::Routine {
            sqlstate: "42602".into(),
            message: format!("invalid routine name `{}`: {error}", stmt.new_name),
        })?;
    if new_schema.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "ALTER ROUTINE RENAME TO requires an unqualified new name".into(),
        });
    }
    let new_name = RelationIdentity::new(&old_identity.schema, new_local_name).qualified_name();
    if registry.get(&new_name).is_some_and(|overloads| {
        overloads
            .iter()
            .any(|function| routine_signature_types(&function.def) == signature)
    }) {
        return Err(SQLError::Routine {
            sqlstate: "42723".into(),
            message: format!(
                "function \"{}\" already exists with same argument types",
                RelationIdentity::from_legacy_name(&new_name)
                    .map_or_else(|_| new_name.clone(), |identity| identity.name)
            ),
        });
    }
    Ok(new_name)
}

pub fn move_routine_registry_entry(
    mut registry: BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    target: &RoutineRenameTarget,
) -> Result<BTreeMap<String, Vec<Arc<SQLUserFunction>>>, SQLError> {
    let old_overloads = registry.get_mut(&target.old_name).ok_or_else(|| {
        SQLError::Internal(format!(
            "resolved ALTER routine registry entry `{}` disappeared before rename",
            target.old_name
        ))
    })?;
    let renamed = old_overloads.remove(target.position);
    if old_overloads.is_empty() {
        registry.remove(&target.old_name);
    }
    let mut renamed_definition = renamed.def.clone();
    renamed_definition.name.clone_from(&target.new_name);
    let new_overloads = registry.entry(target.new_name.clone()).or_default();
    new_overloads.push(Arc::new(SQLUserFunction {
        def: renamed_definition,
        compiled: renamed.compiled.clone(),
    }));
    new_overloads.sort_by(|left, right| {
        routine_signature_types(&left.def)
            .cmp(&routine_signature_types(&right.def))
            .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
    });
    Ok(registry)
}

pub fn rewrite_routine_owned_dependency_identity(
    def: &mut CreateFunction,
    target: &FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    for default in def
        .params
        .iter_mut()
        .filter_map(|parameter| parameter.default.as_mut())
    {
        changed |= crate::catalog::stored_ast::rewrite_expression_routine_identity(
            default, target, new_name,
        )?;
    }
    if let FunctionBody::Statements(statements) = &mut def.body {
        for statement in statements {
            changed |= crate::catalog::stored_ast::rewrite_statement_routine_identity(
                statement, target, new_name,
            )?;
        }
    }
    Ok(changed)
}
