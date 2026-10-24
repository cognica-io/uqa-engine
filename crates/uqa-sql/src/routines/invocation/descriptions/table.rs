//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The table caller retains a successful declaration, never its returned rows.

use super::{Produced, ProductionControl, RoutineCallDescription, RoutineConversionEnvironment};
use crate::{
    ast::CreateFunction,
    routines::{
        declaration::RoutineTypeCatalog,
        result_check::{declared_sql_function_result, SQLFunctionResultLayout},
    },
    SQLError,
};
use std::borrow::Cow;
use uqa_core::ValueRetentionError;

impl RoutineCallDescription {
    /// Resolve the selected routine's declared table shape at the original call
    /// boundary. Caller-specific anonymous-record checks and every value stay live.
    pub fn table_result_layout<'a>(
        &'a self,
        types: &dyn RoutineTypeCatalog,
        definition: &CreateFunction,
        environment: RoutineConversionEnvironment<'_>,
    ) -> Result<Cow<'a, SQLFunctionResultLayout>, SQLError> {
        environment.control.check()?;
        let reusable = self.is_current((environment.scope)().as_ref());
        if reusable {
            if let Some(saved) = self.table_result.get() {
                return Ok(Cow::Borrowed(saved));
            }
        }
        let layout = declared_sql_function_result(types, definition)?;
        if reusable
            && self.table_result.get().is_none()
            && super::body::fixed_result(definition)
            && self.is_current((environment.scope)().as_ref())
        {
            match retained_layout(&layout, &environment.control) {
                Ok(saved) => {
                    if self.table_result.set(saved).is_ok() {
                        return Ok(Cow::Borrowed(
                            self.table_result.get().expect("published table result"),
                        ));
                    }
                }
                Err(ValueRetentionError::Memory(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Cow::Owned(layout))
    }
}

fn retained_layout(
    layout: &SQLFunctionResultLayout,
    control: &ProductionControl<'_>,
) -> Result<Box<Produced<SQLFunctionResultLayout>>, ValueRetentionError> {
    let allocation = control.reserve(size_of::<Produced<SQLFunctionResultLayout>>())?;
    let (layout, memory) = layout.clone_with_control(control)?.into_parts();
    Ok(Box::new(
        control.finish(layout, control.combine(allocation, memory))?,
    ))
}
