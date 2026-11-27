//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine rename identities, collision checks, registry movement, and stored AST rewrites.

use super::RoutineRegistry;
use crate::routines::compare_routine_overloads;
use crate::{
    ast::{CreateFunction, FunctionBinding, FunctionBody, RenameRoutineStmt},
    routines::{routine_signature_types, SQLUserFunction},
    SQLError,
};
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
    destination: RelationIdentity,
    registry: &RoutineRegistry,
    catalog: &dyn super::names::RoutineNameCatalog,
) -> Result<RoutineRenameTarget, SQLError> {
    let signature = routine_signature_types(definition);
    let new_name = destination.qualified_name();
    if (!stmt.set_schema || new_name != old_name)
        && registry.get(&new_name).is_some_and(|overloads| {
            overloads
                .iter()
                .any(|function| routine_signature_types(&function.def) == signature)
        })
    {
        let message = if stmt.set_schema {
            let signature =
                super::routine_signature_display(catalog, &destination.name, &signature);
            format!(
                "function {signature} already exists in schema \"{}\"",
                destination.schema
            )
        } else {
            format!(
                "function \"{}\" already exists with same argument types",
                destination.name
            )
        };
        return Err(SQLError::Routine {
            sqlstate: "42723".into(),
            message,
        });
    }
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
            value_binding: None,
            resolution_error: None,
            input_collation: None,
        },
    })
}

/// Decode the changed identifier component before checking creation privilege on its destination namespace.
pub fn routine_destination(
    stmt: &RenameRoutineStmt,
    old_identity: &RelationIdentity,
) -> Result<RelationIdentity, SQLError> {
    let (qualifier, component) =
        RelationIdentity::parse_reference(&stmt.new_name).map_err(|error| SQLError::Routine {
            sqlstate: "42601".into(),
            message: format!("invalid routine name \"{}\": {error}", stmt.new_name),
        })?;
    if qualifier.is_some() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "ALTER ROUTINE RENAME TO requires an unqualified new name".into(),
        });
    }
    Ok(if stmt.set_schema {
        RelationIdentity::new(component, &old_identity.name)
    } else {
        RelationIdentity::new(&old_identity.schema, component)
    })
}

pub fn move_routine_registry_entry(
    mut registry: crate::routines::registry::RoutineRegistry,
    target: &RoutineRenameTarget,
) -> Result<crate::routines::registry::RoutineRegistry, SQLError> {
    let mut old_overloads = registry.get(&target.old_name).cloned().ok_or_else(|| {
        SQLError::Internal(format!(
            "resolved ALTER routine registry entry `{}` disappeared before rename",
            target.old_name
        ))
    })?;
    let renamed = old_overloads.remove(target.position);
    if old_overloads.is_empty() {
        registry.remove(&target.old_name)?;
    } else {
        registry.insert(target.old_name.clone(), old_overloads)?;
    }
    let mut renamed_definition = renamed.def.clone();
    renamed_definition.name.clone_from(&target.new_name);
    let mut new_overloads = registry.get(&target.new_name).cloned().unwrap_or_default();
    new_overloads.push(SQLUserFunction::new(renamed_definition, renamed.body.clone()).shared());
    new_overloads.sort_by(|left, right| compare_routine_overloads(&left.def, &right.def));
    registry.insert(target.new_name.clone(), new_overloads)?;
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
