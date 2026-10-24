//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Body metadata follows the selected call's original, live conversion boundaries.

use super::{
    conversion_slots, ColumnType, Produced, ProductionControl, RoutineCallDescription,
    RoutineConversionEnvironment, RoutineValueDescription, SQLError, Value, ValueRetentionError,
};
use crate::{
    ast::{CreateFunction, FunctionReturns},
    binding::statements::AnalyzedResult,
    routines::{
        declaration::RoutineTypeCatalog,
        result_check::{
            sql_function_result_layout, SQLFunctionResultKind, SQLFunctionResultLayout,
        },
    },
};
use std::{borrow::Cow, sync::OnceLock};
use uqa_core::memory::MemoryReservation;

pub(super) struct ResultDescription {
    columns: Option<Produced<Vec<Option<ColumnType>>>>,
    record: Option<Produced<Vec<Option<ColumnType>>>>,
    layout: Produced<SQLFunctionResultLayout>,
    conversions: Produced<Vec<OnceLock<Produced<RoutineValueDescription>>>>,
    _memory: Option<MemoryReservation>,
}

fn input_types(result: Option<&AnalyzedResult>) -> Option<&[Option<ColumnType>]> {
    result.and_then(AnalyzedResult::column_types)
}

fn input_record(result: Option<&AnalyzedResult>) -> Option<&[Option<ColumnType>]> {
    result
        .and_then(|result| result.record_fields(0))
        .map(AsRef::as_ref)
}

fn fixed_result(definition: &CreateFunction) -> bool {
    let outputs = definition.output_params();
    if !outputs.is_empty() {
        return outputs
            .iter()
            .all(|parameter| super::fixed_name(&parameter.type_name));
    }
    match &definition.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            super::fixed_name(type_name)
        }
        FunctionReturns::None | FunctionReturns::Table => true,
    }
}

impl ResultDescription {
    fn matches(&self, result: Option<&AnalyzedResult>) -> bool {
        self.columns.as_ref().map(|types| types.as_slice()) == input_types(result)
            && self.record.as_ref().map(|types| types.as_slice()) == input_record(result)
    }

    fn new(
        result: Option<&AnalyzedResult>,
        layout: &SQLFunctionResultLayout,
        control: &ProductionControl<'_>,
    ) -> Result<Box<Self>, ValueRetentionError> {
        let memory = control.reserve(size_of::<Self>())?;
        let columns = input_types(result)
            .map(|types| crate::routines::result_check::retention::copy_types(types, control))
            .transpose()?;
        let record = input_record(result)
            .map(|types| crate::routines::result_check::retention::copy_types(types, control))
            .transpose()?;
        let layout = layout.clone_with_control(control)?;
        let count = if layout.kind == SQLFunctionResultKind::Tuple {
            layout.columns.as_ref().map_or(0, Vec::len)
        } else {
            1
        };
        let conversions = conversion_slots(count, control)?;
        Ok(Box::new(Self {
            columns,
            record,
            layout,
            conversions,
            _memory: memory,
        }))
    }
}

impl RoutineCallDescription {
    /// Resolve a concrete SQL-body input at its original position, after invocation coercion. Only the selected call's exact parameter spelling can reuse this slot.
    pub fn body_parameter_type<'a>(
        &'a self,
        index: usize,
        name: &str,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Cow<'a, ColumnType>, SQLError> {
        environment.control.check()?;
        let invocation = self.binding.invocation.as_ref().expect("retained call");
        let reusable = invocation
            .parameter_types
            .get(index)
            .is_some_and(|ty| ty == name)
            && self.is_current((environment.scope)().as_ref());
        if reusable {
            if let Some(ty) = self.body_parameters[index].get() {
                return Ok(Cow::Borrowed(ty));
            }
        }
        let ty = ColumnType::from_sql_name(name)
            .ok()
            .or_else(|| environment.values.catalog_column_type(name))
            .ok_or_else(|| SQLError::TypeMismatch(format!("unknown type `{name}`")))?;
        if reusable && self.is_current((environment.scope)().as_ref()) {
            match ty.clone_with_control(&environment.control) {
                Ok(copied) => {
                    let _ = self.body_parameters[index].set(copied);
                }
                Err(ValueRetentionError::Memory(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Cow::Owned(ty))
    }

    /// Reuse the selected function's successful result check only for the complete output type/record description in the original scope. Callers still perform caller-specific anonymous-record checks at their original boundary.
    pub fn body_result_layout<'a>(
        &'a self,
        types: &dyn RoutineTypeCatalog,
        definition: &CreateFunction,
        result: Option<&AnalyzedResult>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Cow<'a, SQLFunctionResultLayout>, SQLError> {
        environment.control.check()?;
        let reusable = self.is_current((environment.scope)().as_ref());
        if reusable {
            if let Some(saved) = self.body_result.get().filter(|saved| saved.matches(result)) {
                return Ok(Cow::Borrowed(&saved.layout));
            }
        }
        let layout = sql_function_result_layout(types, definition, result)?;
        if reusable
            && self.body_result.get().is_none()
            && fixed_result(definition)
            && self.is_current((environment.scope)().as_ref())
        {
            match ResultDescription::new(result, &layout, &environment.control) {
                Ok(saved) => {
                    if self.body_result.set(saved).is_ok() {
                        return Ok(Cow::Borrowed(
                            &self
                                .body_result
                                .get()
                                .expect("published result description")
                                .layout,
                        ));
                    }
                }
                Err(ValueRetentionError::Memory(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Cow::Owned(layout))
    }

    /// Convert every returned value with live callbacks. Body effects and earlier output callbacks can invalidate the scope before this conversion.
    pub fn coerce_body_result(
        &self,
        layout: &SQLFunctionResultLayout,
        index: usize,
        value: &Value,
        source: Option<&ColumnType>,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Value, SQLError> {
        environment.control.check()?;
        let target = if layout.kind == SQLFunctionResultKind::Tuple {
            layout
                .columns
                .as_ref()
                .and_then(|columns| columns.get(index))
                .map_or(&layout.declared_type, |column| &column.ty)
        } else {
            &layout.declared_type
        };
        if self.is_current((environment.scope)().as_ref()) {
            if let Some(saved) = self.body_result.get().filter(|saved| {
                std::ptr::eq(
                    std::ptr::from_ref(&*saved.layout),
                    std::ptr::from_ref(layout),
                )
            }) {
                if let Some(slot) = saved.conversions.get(index) {
                    if let Some(description) = slot.get() {
                        return description.coerce(environment.values, value, source);
                    }
                    return self.coerce(&target.catalog_name(), slot, value, source, environment);
                }
            }
        }
        crate::assignment::routines::coerce_routine_value_from(
            environment.values,
            value,
            &target.catalog_name(),
            source,
        )
    }
}
