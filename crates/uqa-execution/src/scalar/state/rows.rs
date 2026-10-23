//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared metadata stays with one prepared row producer and its original lease.

use std::sync::OnceLock;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RecordFieldType, RowDescriptor, ValueRetentionError,
};
use uqa_sql::SQLError;

#[derive(Debug, Default)]
pub(crate) struct RowConstructorState {
    descriptor: OnceLock<Produced<RowDescriptor>>,
}

impl RowConstructorState {
    pub(crate) fn shared(&self, fields: &[RecordFieldType]) -> Option<RowDescriptor> {
        self.descriptor
            .get()
            .filter(|saved| saved.field_types() == fields)
            .map(|saved| (**saved).clone())
    }

    pub(crate) fn remember(
        &self,
        descriptor: &RowDescriptor,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if self.descriptor.get().is_none() {
            match control.reserve(descriptor.retained_buffer_bytes()?) {
                Ok(memory) => {
                    let _ = self
                        .descriptor
                        .set(control.finish(descriptor.clone(), memory)?);
                }
                Err(ValueRetentionError::Memory(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
