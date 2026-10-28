//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A statement's frozen scalar constants share one bounded, encrypted spill owner.

use crate::{
    batch::{PhysicalRow, RowSchema},
    spill::BufferedIndexedSpill,
};
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

struct ConstantRows(Mutex<BufferedIndexedSpill>);

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
            source: Arc::new(ConstantRows(Mutex::new(BufferedIndexedSpill::new(
                RowSchema::new(vec!["constant".into()]),
                bytes,
            )))),
        }
    }
}

impl ConstantValueStorage for RetainedConstants {
    fn retain(&self, value: Value) -> Result<RetainedConstantValue, SQLError> {
        uqa_sql::expr::check_statement_cancellation()?;
        let mut source = self.source.0.lock();
        let index = source.len();
        source
            .push(&PhysicalRow::from_values(vec![value]))
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        drop(source);
        uqa_sql::expr::check_statement_cancellation()?;
        Ok(RetainedConstantValue::new(self.source.clone(), index))
    }
}

impl ConstantValueSource for ConstantRows {
    fn read_with_control(
        &self,
        index: u64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        self.0
            .lock()
            .get_constant_with_control(index, control)
            .map_err(crate::physical::physical_exec_error)
    }

    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        observer.allocation(size_of::<Self>(), size_of::<Self>())?;
        self.0.lock().observe_resident_allocations(observer)
    }

    fn read(&self, index: u64) -> Result<Value, SQLError> {
        let row = self
            .0
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
