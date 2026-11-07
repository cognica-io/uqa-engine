//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared routine definitions and bodies retain their own complete syntax allowances.

use super::{CompiledFunctionBody, RoutineBody, SQLUserFunction};
use crate::{plan::copy::capture_stored_with_control, SQLError};
use std::sync::Arc;
use uqa_core::memory::{MemoryError, ProductionControl};

mod retained;
pub use retained::RetainedRoutineValue;

pub type SQLRoutine = Arc<RetainedRoutineValue<SQLUserFunction>>;
pub type SharedRoutineBody = Arc<RetainedRoutineValue<CompiledFunctionBody>>;

impl CompiledFunctionBody {
    /// An ordinary session compilation keeps the same body with no catalog allowance.
    pub fn shared(self) -> SharedRoutineBody {
        RetainedRoutineValue::native(
            ProductionControl::uncontrolled()
                .finish(self, None)
                .expect("ordinary body sharing has no fallible admission"),
            &ProductionControl::uncontrolled(),
        )
        .expect("ordinary body sharing has no fallible admission")
    }
}

impl SQLUserFunction {
    /// An unpublished definition enters the native catalog through its admitted publication owner.
    pub fn shared(self) -> SQLRoutine {
        RetainedRoutineValue::native(
            ProductionControl::uncontrolled()
                .finish(self, None)
                .expect("ordinary routine sharing has no fallible admission"),
            &ProductionControl::uncontrolled(),
        )
        .expect("ordinary routine sharing has no fallible admission")
    }

    pub(super) fn copy_native(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<SQLRoutine, SQLError> {
        let definition = capture_stored_with_control(&self.def, control)?;
        let body = match &self.body {
            RoutineBody::Source => RoutineBody::Source,
            RoutineBody::Bound(body) => {
                let CompiledFunctionBody::SQL(plans) = &***body else {
                    return Err(SQLError::Internal(
                        "stored routine contains a session-compiled PL/pgSQL body".into(),
                    ));
                };
                if control
                    .budget()
                    .is_some_and(|memory| body.is_retained_by(memory))
                {
                    RoutineBody::Bound(body.clone())
                } else {
                    let (plans, memory) = capture_stored_with_control(plans, control)?.into_parts();
                    RoutineBody::Bound(RetainedRoutineValue::native(
                        control.finish(CompiledFunctionBody::SQL(plans), memory)?,
                        control,
                    )?)
                }
            }
        };
        let (definition, memory) = definition.into_parts();
        RetainedRoutineValue::native(
            control.finish(Self::new(definition, body), memory)?,
            control,
        )
    }
}

pub(super) fn retained_bytes(function: &SQLRoutine) -> Result<usize, SQLError> {
    let body = match &function.body {
        RoutineBody::Source => 0,
        RoutineBody::Bound(body) => body.reserved_bytes(),
    };
    Ok(function
        .reserved_bytes()
        .checked_add(body)
        .ok_or(MemoryError::SizeOverflow)?)
}

pub(super) fn retained_by(function: &SQLRoutine, control: &ProductionControl<'_>) -> bool {
    control.budget().is_some_and(|memory| {
        function.is_retained_by(memory)
            && match &function.body {
                RoutineBody::Source => true,
                RoutineBody::Bound(body) => body.is_retained_by(memory),
            }
    })
}
