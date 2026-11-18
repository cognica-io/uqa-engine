//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind stored callbacks by incarnation in wrapper and handler-before-validator order.

use super::{
    ForeignWrapperDefinition, ForeignWrapperFunction, ForeignWrapperHandler, ForeignWrappers,
};
use crate::{
    catalog::work::inline,
    routines::{registry::RoutineRegistry, signature_matches, SQLRoutine, SignatureWork},
    SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{ProductionControl, ProductionMap},
};

/// Resolve retained handler and validator identities against SQL routines under the caller's limits.
pub fn validate_functions_with_control(
    wrappers: &ForeignWrappers,
    routines: &RoutineRegistry,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(validate(wrappers, routines, control, &mut ()))
}

/// Retain the selected definitions and routine iterator across the caller's native work windows.
pub async fn validate_functions_from_input(
    wrappers: &ForeignWrappers,
    routines: &RoutineRegistry,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate(wrappers, routines, control, input).await
}

fn references(wrapper: &ForeignWrapperDefinition) -> impl Iterator<Item = &ForeignWrapperFunction> {
    let handler = match &wrapper.handler {
        ForeignWrapperHandler::Function(function) => Some(function),
        _ => None,
    };
    handler
        .into_iter()
        .chain(wrapper.validator.as_ref())
        .filter(|reference| !reference.binding.builtin)
}

async fn validate(
    wrappers: &ForeignWrappers,
    routines: &RoutineRegistry,
    control: &ProductionControl<'_>,
    work: &mut impl SignatureWork,
) -> Result<(), SQLError> {
    control.check()?;
    if routines.is_empty() {
        return Ok(());
    }
    validate_groups(
        wrappers,
        routines.values().map(|group| group.iter()),
        control,
        work,
    )
    .await
}

async fn validate_groups<'r, I: Iterator<Item = &'r SQLRoutine> + Send>(
    wrappers: &ForeignWrappers,
    mut groups: impl Iterator<Item = I> + Send,
    control: &ProductionControl<'_>,
    work: &mut impl SignatureWork,
) -> Result<(), SQLError> {
    let mut selected = ProductionMap::new(*control);
    for wrapper in wrappers.values() {
        work.checkpoint().await?;
        control.check()?;
        for callback in references(wrapper) {
            let id = callback.binding.object_id;
            let uses = selected.get(&id).map_or(0usize, |(_, uses)| *uses);
            let uses = uses
                .checked_add(1)
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            selected.insert_copy(id, (None, uses))?;
        }
    }
    if selected.is_empty() {
        return Ok(());
    }
    let mut current_group = None::<I>;
    for wrapper in wrappers.values() {
        work.checkpoint().await?;
        control.check()?;
        for callback in references(wrapper) {
            let id = callback.binding.object_id;
            let (mut found, uses) = *selected.get(&id).expect("collected callback");
            while found.is_none() {
                work.checkpoint().await?;
                control.check()?;
                let Some(function) = current_group.as_mut().and_then(Iterator::next) else {
                    current_group = groups.next();
                    if current_group.is_none() {
                        break;
                    }
                    continue;
                };
                let identity = function.def.object_id;
                if let Some(&(None, remaining)) = selected.get(&identity) {
                    selected.insert_copy(identity, (Some(function), remaining))?;
                }
                if identity == id {
                    found = Some(function);
                }
            }
            if uses == 1 {
                selected.remove(&id);
            } else {
                selected.insert_copy(id, (found, uses - 1))?;
            }
            let Some(function) = found else { continue };
            if function.def.catalog_oid != Some(callback.oid)
                || function.def.is_procedure
                || !signature_matches(
                    &function.def,
                    &callback.binding.argument_types,
                    control,
                    work,
                )
                .await?
            {
                let message = crate::catalog::work::text_parts(
                    work,
                    &[
                        "foreign-data wrapper `",
                        &wrapper.name,
                        "`: live function disagrees with its retained identity",
                    ],
                    control,
                )
                .await?;
                return Err(SQLError::Internal(message.into_parts().0));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
