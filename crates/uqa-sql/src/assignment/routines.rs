//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

mod production;
pub use production::coerce_routine_value_from_with_control;
pub trait RoutineValueContext: super::AssignmentContext {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType>;

    fn catalog_column_type_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        control.check()?;
        self.catalog_column_type(name)
            .map(|ty| ty.retain_external_with_control(control).map_err(Into::into))
            .transpose()
    }
}
pub fn coerce_routine_value(
    context: &dyn RoutineValueContext,
    value: &Value,
    type_name: &str,
) -> Result<Value, SQLError> {
    coerce_routine_value_from(context, value, type_name, None)
}

pub fn coerce_routine_value_from(
    context: &dyn RoutineValueContext,
    value: &Value,
    type_name: &str,
    source: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    let control = ProductionControl::uncontrolled();
    coerce_routine_value_from_with_control(
        context,
        control.copy_value(value)?,
        type_name,
        source,
        &control,
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary routine coercion owner".into()))
}

/// The assignment semantics selected by a routine's declared type. This describes a conversion, never a converted value; callers may retain it only while the catalog and name-resolution scope that selected it remain unchanged.
#[derive(Debug)]
pub struct RoutineValueDescription(RoutineValueKind);

#[derive(Debug)]
enum RoutineValueKind {
    Record,
    Trigger,
    AnyArray,
    RefCursor,
    Void,
    Catalog(ColumnType),
}

impl RoutineValueDescription {
    /// Resolve at the original conversion boundary. An unresolved name keeps the ordinary value-dependent cast path and is not a reusable description.
    pub fn resolve(context: &dyn RoutineValueContext, name: &str) -> Option<Self> {
        Self::resolve_with_control(context, name, &ProductionControl::uncontrolled())
            .expect("ordinary routine descriptor lookup")
            .map(|description| {
                description
                    .into_uncontrolled()
                    .expect("ordinary routine descriptor owner")
            })
    }

    /// Admit a retained copy of the descriptor's owned type payload before copying it.
    pub fn clone_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>, uqa_core::ValueRetentionError> {
        let kind = match &self.0 {
            RoutineValueKind::Record => RoutineValueKind::Record,
            RoutineValueKind::Trigger => RoutineValueKind::Trigger,
            RoutineValueKind::AnyArray => RoutineValueKind::AnyArray,
            RoutineValueKind::RefCursor => RoutineValueKind::RefCursor,
            RoutineValueKind::Void => RoutineValueKind::Void,
            RoutineValueKind::Catalog(target) => {
                let (target, memory) = target.clone_with_control(control)?.into_parts();
                return control.finish(Self(RoutineValueKind::Catalog(target)), memory);
            }
        };
        control.finish(Self(kind), control.empty_reservation())
    }

    /// Apply the original assignment kernel with live input/domain callbacks and session settings. The caller must revalidate its scope after evaluating the input and any preceding callback.
    pub fn coerce(
        &self,
        context: &dyn RoutineValueContext,
        value: &Value,
        source: Option<&ColumnType>,
    ) -> Result<Value, SQLError> {
        let control = ProductionControl::uncontrolled();
        self.coerce_with_control(context, control.copy_value(value)?, source, &control)?
            .into_uncontrolled()
            .map_err(|_| SQLError::Internal("ordinary described coercion owner".into()))
    }
}

#[cfg(test)]
mod tests;
