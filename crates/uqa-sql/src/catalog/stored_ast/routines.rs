//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine-reference operations over stored catalog ASTs.

use super::{BTreeSet, Expr, RelationIdentity, SQLError, Statement, StoredAstVisitor};

pub fn bind_stored_statement_routines(
    statement: &mut Statement,
    references: &[crate::binding::stored_routines::BoundRoutineReference],
) -> Result<bool, SQLError> {
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut references = references.iter();
    let mut changed = false;
    let mut bind = |name: &mut String,
                    binding: Option<&mut Option<crate::ast::FunctionBinding>>| {
        let reference = references.next().ok_or_else(|| {
            SQLError::Internal(format!(
                "stored catalog routine binding has no entry for call `{name}`"
            ))
        })?;
        changed |= apply_routine_reference(
            name,
            binding,
            reference,
            crate::binding::stored_routines::StoredCallOrigin::Definition,
        )?;
        Ok(())
    };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        projection: None,
        ty: None,
        relation: &mut ignore_relation,
        routine: &mut bind,
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

pub fn rewrite_statement_routine_identity(
    statement: &mut Statement,
    target: &crate::ast::FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut rewrite = |name: &mut String,
                       binding: Option<&mut Option<crate::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        let Some(binding) = binding.and_then(Option::as_mut) else {
            return Ok(());
        };
        if crate::routines::function_binding_matches(binding, target) {
            *name = new_name.to_string();
            binding.name = new_name.to_string();
            changed = true;
        }
        Ok(())
    };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        projection: None,
        ty: None,
        relation: &mut ignore_relation,
        routine: &mut rewrite,
    }
    .bind_statement(statement)?;
    Ok(changed)
}

pub fn rewrite_expression_routine_identity(
    expression: &mut Expr,
    target: &crate::ast::FunctionBinding,
    new_name: &str,
) -> Result<bool, SQLError> {
    let mut changed = false;
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut rewrite = |name: &mut String,
                       binding: Option<&mut Option<crate::ast::FunctionBinding>>|
     -> Result<(), SQLError> {
        let Some(binding) = binding.and_then(Option::as_mut) else {
            return Ok(());
        };
        if crate::routines::function_binding_matches(binding, target) {
            *name = new_name.to_string();
            binding.name = new_name.to_string();
            changed = true;
        }
        Ok(())
    };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        projection: None,
        ty: None,
        relation: &mut ignore_relation,
        routine: &mut rewrite,
    }
    .bind_expr(expression, &BTreeSet::new())?;
    Ok(changed)
}

pub fn bind_stored_expression_routines(
    expression: &mut Expr,
    references: &[crate::binding::stored_routines::BoundRoutineReference],
) -> Result<bool, SQLError> {
    let mut ignore_relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
    let mut references = references.iter();
    let mut changed = false;
    let mut bind = |name: &mut String,
                    binding: Option<&mut Option<crate::ast::FunctionBinding>>| {
        let reference = references.next().ok_or_else(|| {
            SQLError::Internal(format!(
                "stored catalog routine binding has no entry for call `{name}`"
            ))
        })?;
        changed |= apply_routine_reference(
            name,
            binding,
            reference,
            crate::binding::stored_routines::StoredCallOrigin::Definition,
        )?;
        Ok(())
    };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        projection: None,
        ty: None,
        relation: &mut ignore_relation,
        routine: &mut bind,
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

pub(super) fn apply_routine_reference(
    name: &mut String,
    binding: Option<&mut Option<crate::ast::FunctionBinding>>,
    reference: &crate::binding::stored_routines::BoundRoutineReference,
    origin: crate::binding::stored_routines::StoredCallOrigin,
) -> Result<bool, SQLError> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let (changed, _) = crate::catalog::work::inline(apply_routine_reference_from(
        name,
        binding,
        reference,
        origin,
        &control,
        &mut (),
    ))?
    .into_parts();
    Ok(changed)
}

pub(super) async fn apply_routine_reference_from<
    W: crate::plan::ScalarInputWork + crate::catalog::work::Work,
>(
    name: &mut String,
    mut binding: Option<&mut Option<crate::ast::FunctionBinding>>,
    reference: &crate::binding::stored_routines::BoundRoutineReference,
    origin: crate::binding::stored_routines::StoredCallOrigin,
    control: &uqa_core::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<uqa_core::memory::Produced<bool>, SQLError>
where
    W::Error: Into<SQLError>,
{
    use crate::catalog::work::ReferenceWork;
    use uqa_core::RelationReferenceInputError;
    let mut input = ReferenceWork { control, work };
    let source = RelationIdentity::try_borrowed_reference_from(name, &mut input)
        .await
        .map_err(|err| match err {
            RelationReferenceInputError::Input(err) => err,
            RelationReferenceInputError::Reference(err) => {
                SQLError::Internal(format!("decode stored catalog routine `{name}`: {err}"))
            }
        })?;
    let selected = RelationIdentity::try_borrowed_reference_from(&reference.name, &mut input)
        .await
        .map_err(|err| match err {
            RelationReferenceInputError::Input(err) => err,
            RelationReferenceInputError::Reference(err) => SQLError::Internal(format!(
                "decode bound catalog routine `{}`: {err}",
                reference.name
            )),
        })?;
    {
        let mut source_chars = source.name.chars();
        let mut selected_chars = selected.name.chars();
        let mut position = 0;
        loop {
            if position == 0 {
                input.checkpoint().await?;
            }
            match (source_chars.next(), selected_chars.next()) {
                (None, None) => break,
                (Some(left), Some(right)) if left == right => {}
                _ => {
                    return Err(SQLError::Internal(format!(
                        "stored catalog routine call `{name}` does not match bound call `{}`",
                        reference.name
                    )))
                }
            }
            position = (position + 1) % 1024;
        }
    }
    let mut memory = control.empty_reservation();
    let Some(exact) = &reference.binding else {
        return Ok(control.finish(false, memory)?);
    };
    if source.schema.is_none()
        && origin.keeps_unqualified_runtime_lookup(
            binding.as_ref().and_then(|slot| slot.as_ref()),
            exact,
        )
    {
        return Ok(control.finish(false, memory)?);
    }
    let rename = !exact.builtin && name != &exact.name;
    if rename {
        let (spelling, lease) = crate::catalog::work::Work::text(input.work, &exact.name, control)
            .await?
            .into_parts();
        *name = spelling;
        memory = control.combine(memory, lease);
    }
    let rebind = binding
        .as_ref()
        .is_some_and(|slot| slot.as_ref() != Some(exact));
    if rebind {
        let (selected, receipt) = exact
            .clone_with_input_work(control, input.work)
            .await
            .map_err(Into::into)?
            .into_parts();
        *binding.take().expect("binding selected for replacement") = Some(selected);
        memory = control.combine(memory, receipt);
    }
    Ok(control.finish(rename || rebind, memory)?)
}
