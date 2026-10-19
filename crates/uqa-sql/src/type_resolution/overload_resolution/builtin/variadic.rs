//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A borrowed expanded signature reuses ordinary overload scoring without copying declarations.

use super::{
    builtin_canonical_type_name, canonical_column_type_name_with_control,
    match_signature_with_control, BorrowedMatch, BuiltinFunctionOverload, ColumnType,
    MatchedFunctionSignature, Produced, ProductionControl, ProductionVec, RoutineCallDescriptor,
    SignatureParameters, ValueRetentionError,
};
use crate::ast::{RoutineInvocationBinding, RoutineVariadicMode};

struct Expanded<'a> {
    builtin: &'a BuiltinFunctionOverload,
    element: &'a ColumnType,
    count: usize,
}

impl SignatureParameters for Expanded<'_> {
    fn accepts_polymorphic_types(&self) -> bool {
        self.builtin.accepts_polymorphic_types()
    }
    fn parameter_count(&self) -> usize {
        self.count
    }
    fn name(&self, index: usize) -> Option<&str> {
        self.builtin
            .argument_names
            .get(index)
            .and_then(Option::as_deref)
    }
    fn has_default(&self, index: usize) -> bool {
        self.builtin
            .has_default(index.min(self.builtin.argument_types.len() - 1))
    }
    fn canonical_type(
        &self,
        index: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        if index + 1 >= self.builtin.argument_types.len() {
            canonical_column_type_name_with_control(self.element, control)
        } else {
            self.builtin.canonical_type(index, control)
        }
    }
    fn builtin_canonical_type(&self, index: usize) -> Option<&'static str> {
        if index + 1 >= self.builtin.argument_types.len() {
            builtin_canonical_type_name(self.element)
        } else {
            self.builtin.builtin_canonical_type(index)
        }
    }
}

pub(super) fn match_call(
    builtin: &BuiltinFunctionOverload,
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<MatchedFunctionSignature>>, ValueRetentionError> {
    control.check()?;
    if !builtin.variadic {
        if call.explicit_variadic && call.argument_names.iter().any(Option::is_some) {
            return Ok(None);
        }
        return match_signature_with_control(
            builtin,
            call.argument_names,
            call.argument_types,
            control,
        );
    }
    let Some(ColumnType::Array(element)) = builtin.argument_types.last() else {
        return Ok(None);
    };
    if call.explicit_variadic {
        return match_signature_with_control(
            builtin,
            call.argument_names,
            call.argument_types,
            control,
        );
    }
    // PostgreSQL only accepts named notation for a variadic declaration when the array is explicit.
    if call.argument_names.iter().any(Option::is_some) {
        return Ok(None);
    }
    let parameters = Expanded {
        builtin,
        element,
        count: builtin.argument_types.len().max(call.argument_types.len()),
    };
    let Some(matched) = match_signature_with_control(
        &parameters,
        call.argument_names,
        call.argument_types,
        control,
    )?
    else {
        return Ok(None);
    };
    let (mut matched, memory) = matched.into_parts();
    for position in &mut matched.argument_positions {
        control.check()?;
        *position = (*position).min(builtin.argument_types.len() - 1);
    }
    control.finish(matched, memory).map(Some)
}

pub(super) fn invocation(
    matched: &BorrowedMatch<'_>,
    actual: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Box<RoutineInvocationBinding>>>, ValueRetentionError> {
    if !matched.builtin.variadic {
        return control.finish(None, control.empty_reservation());
    }
    let mut positions = ProductionVec::new(*control);
    positions.reserve(matched.signature.argument_positions.len())?;
    for &position in &matched.signature.argument_positions {
        positions.push_copy(position)?;
    }
    let mut targets = ProductionVec::new(*control);
    targets.reserve(matched.signature.argument_types.len())?;
    for target in &matched.signature.argument_types {
        targets.push_produced(control.copy_text(target)?)?;
    }
    let mut sources = ProductionVec::new(*control);
    sources.reserve(actual.len())?;
    for source in actual {
        let value = match source {
            Some(source) => {
                let (source, memory) =
                    canonical_column_type_name_with_control(source, control)?.into_parts();
                control.finish(Some(source), memory)?
            }
            None => control.finish(None, control.empty_reservation())?,
        };
        sources.push_produced(value)?;
    }
    let mut parameters = ProductionVec::new(*control);
    parameters.reserve(matched.builtin.argument_types.len())?;
    for parameter in &matched.builtin.argument_types {
        parameters.push_produced(canonical_column_type_name_with_control(parameter, control)?)?;
    }
    let result = canonical_column_type_name_with_control(&matched.builtin.return_type, control)?;
    let memory = control.reserve(size_of::<RoutineInvocationBinding>())?;
    let (argument_positions, extra) = positions.finish()?.into_parts();
    let memory = control.combine(memory, extra);
    let (argument_targets, extra) = targets.finish()?.into_parts();
    let memory = control.combine(memory, extra);
    let (argument_sources, extra) = sources.finish()?.into_parts();
    let memory = control.combine(memory, extra);
    let (parameter_types, extra) = parameters.finish()?.into_parts();
    let memory = control.combine(memory, extra);
    let (return_type, extra) = result.into_parts();
    let memory = control.combine(memory, extra);
    let parameter_index = matched.builtin.argument_types.len() - 1;
    control.finish(
        Some(Box::new(RoutineInvocationBinding {
            argument_positions,
            argument_targets,
            argument_sources,
            parameter_types,
            return_type: Some(return_type),
            variadic_mode: if matched.explicit_variadic {
                RoutineVariadicMode::Explicit { parameter_index }
            } else {
                RoutineVariadicMode::Expanded { parameter_index }
            },
        })),
        memory,
    )
}
