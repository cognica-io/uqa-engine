//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement's frozen scalar constants share one bounded, encrypted spill owner.

mod production;

use crate::{batch::RowSchema, spill::BufferedIndexedSpill};
use parking_lot::Mutex;
use std::{fmt, sync::Arc};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{
    ir::RetainedScalarLiteral,
    ir::{ConstantValueSource, ConstantValueStorage, RetainedConstantValue},
    SQLError,
};

#[derive(Clone)]
pub struct RetainedConstants {
    source: Arc<ConstantRows>,
}

struct ConstantRows {
    rows: Mutex<BufferedIndexedSpill>,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl fmt::Debug for ConstantRows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConstantRows").finish_non_exhaustive()
    }
}

impl fmt::Debug for RetainedConstants {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedConstants").finish_non_exhaustive()
    }
}

impl RetainedConstants {
    pub fn new(bytes: usize) -> Self {
        Self {
            source: Arc::new(ConstantRows {
                rows: Mutex::new(BufferedIndexedSpill::new(
                    RowSchema::new(vec!["constant".into()]),
                    bytes,
                )),
                _memory: None,
            }),
        }
    }
}

impl ConstantValueStorage for RetainedConstants {
    fn retain(&self, value: Value) -> Result<RetainedConstantValue, SQLError> {
        let control = ProductionControl::uncontrolled();
        self.retain_produced(control.finish(value, None)?, &control)
    }

    fn retain_produced(
        &self,
        value: Produced<Value>,
        control: &ProductionControl<'_>,
    ) -> Result<RetainedConstantValue, SQLError> {
        control.check()?;
        let mut source = self.source.rows.lock();
        let index = source.len();
        source
            .push_constant_with_control(&value, control)
            .map_err(crate::physical::physical_exec_error)?;
        // Publication checked cancellation inside its resident or rollback-capable disk boundary. The successful result owns the just-published identity; no later admission or cancellation can turn it into an unreachable row.
        Ok(RetainedConstantValue::new(
            Arc::clone(&self.source) as Arc<dyn ConstantValueSource>,
            index,
        ))
    }
}

impl ConstantValueSource for ConstantRows {
    fn read_with_control(
        &self,
        index: u64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        self.rows
            .lock()
            .get_constant_with_control(index, control)
            .map_err(crate::physical::physical_exec_error)
    }

    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        observer.allocation(size_of::<Self>(), size_of::<Self>())?;
        self.rows.lock().observe_resident_allocations(observer)
    }

    fn read(&self, index: u64) -> Result<Value, SQLError> {
        let row = self
            .rows
            .lock()
            .get(index)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        let mut values = row.into_physical_values();
        if values.len() != 1 {
            return Err(SQLError::Internal(
                "retained constant storage has invalid row width".into(),
            ));
        }
        Ok(values.pop().expect("validated constant row"))
    }
}

pub(super) fn evaluate(
    literal: &RetainedScalarLiteral,
    context: &super::ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let value = literal.value.read_with_control(control)?;
    let Some(declaration) = &literal.declaration else {
        return Ok(value);
    };
    uqa_sql::expr::composites::literal::evaluate_owned_with_control(
        value,
        declaration,
        context.function_hook(),
        control,
    )
}

#[cfg(test)]
mod tests;
