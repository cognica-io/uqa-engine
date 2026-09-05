//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine-reference operations over stored catalog ASTs.

use super::{BTreeSet, Expr, RelationIdentity, SQLError, Statement, StoredAstVisitor};

pub(crate) fn bind_stored_statement_routines(
    statement: &mut Statement,
    references: &[crate::sql::BoundRoutineReference],
) -> Result<bool, SQLError> {
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut references = references.iter();
    let mut changed = false;
    let mut bind =
        |name: &mut String, binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>| {
            let reference = references.next().ok_or_else(|| {
                SQLError::Internal(format!(
                    "stored catalog routine binding has no entry for call `{name}`"
                ))
            })?;
            changed |= apply_routine_reference(name, binding, reference)?;
            Ok(())
        };
    StoredAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut bind,
    }
    .bind_statement(statement)?;
    if let Some(reference) = references.next() {
        return Err(SQLError::Internal(format!(
            "stored catalog routine binding entry `{}` has no matching call",
            reference.name
        )));
    }
    Ok(changed)
}

pub(crate) fn rewrite_statement_routine_identity(
    statement: &mut Statement,
    target: &uqa_sql::ast::FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut rewrite = |name: &mut String,
                       binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        let Some(binding) = binding.and_then(Option::as_mut) else {
            return Ok(());
        };
        if crate::engine_session::function_binding_matches(binding, target) {
            *name = new_name.to_string();
            binding.name = new_name.to_string();
            changed = true;
        }
        Ok(())
    };
    StoredAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut rewrite,
    }
    .bind_statement(statement)?;
    Ok(changed)
}

pub(crate) fn rewrite_expression_routine_identity(
    expression: &mut Expr,
    target: &uqa_sql::ast::FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut rewrite = |name: &mut String,
                       binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        let Some(binding) = binding.and_then(Option::as_mut) else {
            return Ok(());
        };
        if crate::engine_session::function_binding_matches(binding, target) {
            *name = new_name.to_string();
            binding.name = new_name.to_string();
            changed = true;
        }
        Ok(())
    };
    StoredAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut rewrite,
    }
    .bind_expr(expression, &BTreeSet::new())?;
    Ok(changed)
}

pub(crate) fn bind_stored_expression_routines(
    expression: &mut Expr,
    references: &[crate::sql::BoundRoutineReference],
) -> Result<bool, SQLError> {
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut references = references.iter();
    let mut changed = false;
    let mut bind =
        |name: &mut String, binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>| {
            let reference = references.next().ok_or_else(|| {
                SQLError::Internal(format!(
                    "stored catalog routine binding has no entry for call `{name}`"
                ))
            })?;
            changed |= apply_routine_reference(name, binding, reference)?;
            Ok(())
        };
    StoredAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut bind,
    }
    .bind_expr(expression, &BTreeSet::new())?;
    if let Some(reference) = references.next() {
        return Err(SQLError::Internal(format!(
            "stored catalog routine binding entry `{}` has no matching call",
            reference.name
        )));
    }
    Ok(changed)
}

pub(crate) fn statement_references_routine_identity(
    statement: &Statement,
    target: &uqa_sql::ast::FunctionBinding,
) -> Result<bool, SQLError> {
    let mut statement = statement.clone();
    let mut found = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut inspect = |_: &mut String,
                       binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        found |= binding
            .and_then(|binding| binding.as_ref())
            .is_some_and(|binding| {
                crate::engine_session::function_binding_matches(binding, target)
            });
        Ok(())
    };
    StoredAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut inspect,
    }
    .bind_statement(&mut statement)?;
    Ok(found)
}

pub(crate) fn expression_references_routine_identity(
    expression: &Expr,
    target: &uqa_sql::ast::FunctionBinding,
) -> Result<bool, SQLError> {
    let mut expression = expression.clone();
    let mut found = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut inspect = |_: &mut String,
                       binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        found |= binding
            .and_then(|binding| binding.as_ref())
            .is_some_and(|binding| {
                crate::engine_session::function_binding_matches(binding, target)
            });
        Ok(())
    };
    StoredAstVisitor {
        visit_relation: &mut ignore_relation,
        visit_routine: &mut inspect,
    }
    .bind_expr(&mut expression, &BTreeSet::new())?;
    Ok(found)
}

fn apply_routine_reference(
    name: &mut String,
    binding: Option<&mut Option<uqa_sql::ast::FunctionBinding>>,
    reference: &crate::sql::BoundRoutineReference,
) -> Result<bool, SQLError> {
    let (_, local_name) = RelationIdentity::parse_reference(name).map_err(|error| {
        SQLError::Internal(format!("decode stored catalog routine `{name}`: {error}"))
    })?;
    let (_, reference_local_name) =
        RelationIdentity::parse_reference(&reference.name).map_err(|error| {
            SQLError::Internal(format!(
                "decode bound catalog routine `{}`: {error}",
                reference.name
            ))
        })?;
    if local_name != reference_local_name {
        return Err(SQLError::Internal(format!(
            "stored catalog routine call `{name}` does not match bound call `{}`",
            reference.name
        )));
    }
    let Some(exact) = &reference.binding else {
        return Ok(false);
    };
    let mut changed = false;
    if !exact.builtin && name != &exact.name {
        name.clone_from(&exact.name);
        changed = true;
    }
    if let Some(binding) = binding {
        if binding.as_ref() != Some(exact) {
            *binding = Some(exact.clone());
            changed = true;
        }
    }
    Ok(changed)
}
