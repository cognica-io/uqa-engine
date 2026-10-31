//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{FunctionBindingParts, OwnedBindingCall};
use crate::{ast::FunctionResolutionError, ColumnType, SQLError};
use uqa_core::memory::ProductionControl;

impl OwnedBindingCall {
    pub(in crate::type_resolution) fn undefined(
        self,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let signature =
            super::unresolved_call_signature_with_control(self.name(), names, types, control)?;
        let (signature, memory) = signature.into_parts();
        let error = control.finish(
            FunctionResolutionError::UndefinedFunction { signature },
            memory,
        )?;
        let binding = FunctionBindingParts {
            object_id: None,
            name: control.copy_text(self.name())?,
            argument_types: control.finish(Vec::new(), control.empty_reservation())?,
            builtin: false,
            dispatch: None,
            invocation: None,
            value_binding: None,
            resolution_error: Some(error),
            input_collation: None,
        }
        .finish(control)?;
        self.replace_binding(Some(binding), control)
    }
}
