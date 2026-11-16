//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered argument rules are shared by declaration binding and immutable catalog validation.

use super::routine_definition_error;
use crate::{
    ast::{FunctionParam, FunctionParamMode},
    SQLError,
};
use uqa_core::memory::ProductionControl;

#[derive(Default)]
pub(super) struct ParameterLayout {
    have_defaults: bool,
    after_variadic: bool,
}

impl ParameterLayout {
    pub(super) fn visit(
        &mut self,
        previous: &[FunctionParam],
        parameter: &FunctionParam,
        procedure: bool,
        variadic_array: impl FnOnce() -> Result<bool, SQLError>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let input = !matches!(
            parameter.mode,
            FunctionParamMode::Out | FunctionParamMode::Table
        );
        let output = !matches!(
            parameter.mode,
            FunctionParamMode::In | FunctionParamMode::Variadic
        );
        if input && self.after_variadic {
            return Err(routine_definition_error(
                "VARIADIC parameter must be the last input parameter",
            ));
        }
        if output && procedure && self.after_variadic {
            return Err(routine_definition_error(
                "VARIADIC parameter must be the last parameter",
            ));
        }
        if parameter.mode == FunctionParamMode::Variadic {
            self.after_variadic = true;
            if !variadic_array()? {
                return Err(routine_definition_error(
                    "VARIADIC parameter must be an array",
                ));
            }
        }
        if !parameter.name.is_empty() {
            for earlier in previous {
                control.check()?;
                if earlier.name == parameter.name && names_conflict(parameter.mode, earlier.mode) {
                    return Err(routine_definition_error(format!(
                        "parameter name \"{}\" used more than once",
                        parameter.name
                    )));
                }
            }
        }
        if parameter.default.is_some() {
            if !input {
                return Err(routine_definition_error(
                    "only input parameters can have default values",
                ));
            }
            self.have_defaults = true;
        } else if input && self.have_defaults {
            return Err(routine_definition_error(
                "input parameters after one with a default value must also have defaults",
            ));
        } else if procedure && self.have_defaults {
            return Err(routine_definition_error(
                "procedure OUT parameters cannot appear after one with a default value",
            ));
        }
        Ok(())
    }
}

fn names_conflict(current: FunctionParamMode, earlier: FunctionParamMode) -> bool {
    let input = |mode| matches!(mode, FunctionParamMode::In | FunctionParamMode::Variadic);
    let output = |mode| matches!(mode, FunctionParamMode::Out | FunctionParamMode::Table);
    !(input(current) && output(earlier) || input(earlier) && output(current))
}
