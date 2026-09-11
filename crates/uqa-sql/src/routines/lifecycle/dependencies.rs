//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routine/schema references and atomic registry removal candidates.

use super::{RoutineDropResolution, RoutineDropTarget, RoutineRegistry, RoutineSchemaDependents};
use crate::{
    ast::{CreateFunction, FunctionBody},
    routines::{routine_signature_types, SQLUserFunction},
    SQLError,
};
use std::{collections::BTreeMap, sync::Arc};

pub fn append_schema_function_dependents(
    table_name: &str,
    columns: &[crate::ast::ColumnDef],
    checks: &[crate::ast::TableCheck],
    target: &crate::ast::FunctionBinding,
    foreign: bool,
    dependents: &mut RoutineSchemaDependents,
) -> Result<(), SQLError> {
    let relation = if foreign {
        format!("foreign table `{table_name}`")
    } else {
        format!("`{table_name}`")
    };
    for column in columns {
        if let Some(generated) = &column.generated {
            let referenced =
                generated.function_dependencies.iter().any(|dependency| {
                    crate::routines::function_binding_matches(dependency, target)
                }) || crate::catalog::stored_ast::expression_references_routine_identity(
                    &generated.expression,
                    target,
                )?;
            if referenced {
                dependents
                    .columns
                    .push((table_name.to_string(), column.name.clone(), foreign));
            }
        }
        if let Some(default) = &column.default {
            if crate::catalog::stored_ast::expression_references_routine_identity(default, target)?
            {
                dependents
                    .defaults
                    .push((table_name.to_string(), column.name.clone(), foreign));
            }
        }
        if let Some(check) = &column.check {
            if crate::catalog::stored_ast::expression_references_routine_identity(check, target)? {
                let name = column.check_name.clone().ok_or_else(|| {
                    SQLError::Internal(format!(
                        "CHECK constraint on {relation}.`{}` has no catalog name",
                        column.name
                    ))
                })?;
                dependents
                    .checks
                    .push((table_name.to_string(), name, foreign));
            }
        }
    }
    for check in checks {
        if crate::catalog::stored_ast::expression_references_routine_identity(&check.expr, target)?
        {
            let name = check.name.clone().ok_or_else(|| {
                SQLError::Internal(format!(
                    "table CHECK constraint on {relation} has no catalog name"
                ))
            })?;
            dependents
                .checks
                .push((table_name.to_string(), name, foreign));
        }
    }
    Ok(())
}

pub fn stored_routine_dependents(
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    target: &RoutineDropTarget,
) -> Result<Vec<RoutineDropTarget>, SQLError> {
    let binding = target.binding();
    let mut dependents = Vec::new();
    for (name, overloads) in registry {
        for function in overloads {
            if routine_definition_references(&function.def, &binding)? {
                dependents.push(RoutineDropTarget {
                    object_id: function.def.object_id,
                    name: name.clone(),
                    argument_types: routine_signature_types(&function.def),
                    is_procedure: function.def.is_procedure,
                });
            }
        }
    }
    dependents.retain(|dependent| dependent != target);
    dependents.sort();
    dependents.dedup();
    Ok(dependents)
}

pub fn routine_definition_references(
    def: &CreateFunction,
    target: &crate::ast::FunctionBinding,
) -> Result<bool, SQLError> {
    for default in def
        .params
        .iter()
        .filter_map(|parameter| parameter.default.as_ref())
    {
        if crate::catalog::stored_ast::expression_references_routine_identity(default, target)? {
            return Ok(true);
        }
    }
    let FunctionBody::Statements(statements) = &def.body else {
        return Ok(false);
    };
    for statement in statements {
        if crate::catalog::stored_ast::statement_references_routine_identity(statement, target)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn expand_stored_routine_drop_dependents(
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    cascade: bool,
    resolution: &mut RoutineDropResolution,
    display_label: &dyn Fn(&RoutineDropTarget) -> Result<String, SQLError>,
) -> Result<Vec<RoutineDropTarget>, SQLError> {
    let explicit_targets = resolution.seen_targets.clone();
    let mut cascaded_routines = Vec::new();
    let mut target_index = 0;
    while target_index < resolution.targets.len() {
        let target = resolution.targets[target_index].clone();
        target_index += 1;
        if target.is_procedure {
            continue;
        }
        for dependent in stored_routine_dependents(registry, &target)? {
            if explicit_targets.contains(&dependent) || resolution.seen_targets.contains(&dependent)
            {
                continue;
            }
            if !cascade {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "cannot drop function {} because other objects depend on it",
                        display_label(&target)?
                    ),
                });
            }
            resolution.seen_targets.insert(dependent.clone());
            cascaded_routines.push(dependent.clone());
            resolution.targets.push(dependent);
        }
    }
    Ok(cascaded_routines)
}

pub fn remove_routine_registry_targets(
    next: &mut RoutineRegistry,
    targets: &[RoutineDropTarget],
) -> Result<(), SQLError> {
    // Revalidate every target before mutating `next`. This retains a concurrently registered unrelated overload and keeps a multi-target DROP all-or-nothing if any preflighted identity has disappeared.
    for target in targets {
        let overloads = next.get(&target.name).ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved {} registry entry `{}` disappeared before DROP",
                target.kind(),
                target.name
            ))
        })?;
        if !overloads.iter().any(|function| {
            function.def.is_procedure == target.is_procedure
                && routine_signature_types(&function.def) == target.argument_types
        }) {
            return Err(SQLError::Internal(format!(
                "resolved {} {} disappeared before DROP",
                target.kind(),
                target.label()
            )));
        }
    }

    for target in targets.iter().rev() {
        let overloads = next.get_mut(&target.name).ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved {} registry entry `{}` disappeared while applying DROP",
                target.kind(),
                target.name
            ))
        })?;
        let position = overloads
            .iter()
            .position(|function| {
                function.def.is_procedure == target.is_procedure
                    && routine_signature_types(&function.def) == target.argument_types
            })
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved {} {} disappeared while applying DROP",
                    target.kind(),
                    target.label()
                ))
            })?;
        overloads.remove(position);
        if overloads.is_empty() {
            next.remove(&target.name);
        }
    }
    Ok(())
}
