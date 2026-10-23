//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One admitted name descriptor per prepared composite constructor.

use crate::{expr::composites::CompositeTypeDescriptor, SQLError};
use std::sync::OnceLock;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RecordDescriptor, ValueRetentionError,
};

#[derive(Debug, Default)]
pub struct CompositeConstructorState {
    descriptor: OnceLock<Produced<RecordDescriptor>>,
}

impl CompositeConstructorState {
    pub(super) fn shared(&self, current: &CompositeTypeDescriptor) -> Option<RecordDescriptor> {
        let saved = self.descriptor.get()?;
        (saved.type_oid() == Some(current.type_oid)
            && saved.field_names().iter().map(String::as_str).eq(current
                .attributes
                .iter()
                .map(|attribute| attribute.name.as_str())))
        .then(|| (**saved).clone())
    }

    pub(super) fn remember(
        &self,
        descriptor: &RecordDescriptor,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if self.descriptor.get().is_none() {
            // Cache admission is optional; failure must not reject an already
            // produced row. Cancellation remains an error, never a cache miss.
            match control.reserve(descriptor.retained_payload_bytes()?) {
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
