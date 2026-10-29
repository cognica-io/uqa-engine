//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The constant store and its factory adapter retain independent construction leases.

use super::{
    Arc, BufferedIndexedSpill, ConstantRows, ConstantValueStorage, Mutex, Produced,
    ProductionControl, RetainedConstantValue, RetainedConstants, RowSchema, SQLError, Value,
};
use uqa_core::memory::ProductionVec;

#[derive(Debug)]
struct ControlledStorage(Produced<RetainedConstants>);

impl RetainedConstants {
    pub fn shared_with_control(
        bytes: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<dyn ConstantValueStorage>, SQLError> {
        control.check()?;
        let mut columns = ProductionVec::new(*control);
        columns.push_produced(control.copy_text("constant")?)?;
        let schema = RowSchema::new_with_control(columns.finish()?, control)?;
        let source_memory = control.reserve(size_of::<ConstantRows>())?;
        let source = Arc::new(ConstantRows {
            rows: Mutex::new(BufferedIndexedSpill::new(schema, bytes)),
            _memory: source_memory,
        });
        let adapter_memory = control.reserve(size_of::<ControlledStorage>())?;
        let storage = control.finish(Self { source }, adapter_memory)?;
        Ok(Arc::new(ControlledStorage(storage)))
    }
}

impl ConstantValueStorage for ControlledStorage {
    fn retain(&self, value: Value) -> Result<RetainedConstantValue, SQLError> {
        self.0.retain(value)
    }

    fn retain_produced(
        &self,
        value: Produced<Value>,
        control: &ProductionControl<'_>,
    ) -> Result<RetainedConstantValue, SQLError> {
        self.0.retain_produced(value, control)
    }
}
