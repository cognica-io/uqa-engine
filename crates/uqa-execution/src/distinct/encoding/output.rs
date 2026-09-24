//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical writers preserve typed resource failures across normalization.

use super::{encode_bytes, BudgetedVec, ExecError, ExecResult, StorageReadControl};

pub(super) trait KeyOutput {
    fn push_byte(&mut self, value: u8) -> ExecResult<()>;
    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()>;

    fn control(&self) -> Option<&StorageReadControl> {
        None
    }

    fn check(&self) -> ExecResult<()> {
        if let Some(control) = self.control() {
            control.check().map_err(resource_error)?;
        }
        Ok(())
    }
}

pub(super) struct BudgetedOutput<'a> {
    values: BudgetedVec<u8>,
    control: &'a StorageReadControl,
}

impl<'a> BudgetedOutput<'a> {
    pub(super) fn new(control: &'a StorageReadControl) -> Self {
        Self {
            values: BudgetedVec::new(control.memory()),
            control,
        }
    }

    pub(super) fn into_values(self) -> BudgetedVec<u8> {
        self.values
    }
}

impl KeyOutput for BudgetedOutput<'_> {
    fn push_byte(&mut self, value: u8) -> ExecResult<()> {
        self.check()?;
        self.values.push(value).map_err(resource_error)
    }

    fn extend_bytes(&mut self, values: &[u8]) -> ExecResult<()> {
        for chunk in values.chunks(4096) {
            self.check()?;
            self.values
                .extend_from_slice(chunk)
                .map_err(resource_error)?;
        }
        Ok(())
    }

    fn control(&self) -> Option<&StorageReadControl> {
        Some(self.control)
    }
}

pub(super) fn encode_jsonb(value: &str, output: &mut impl KeyOutput) -> ExecResult<()> {
    output.push_byte(9)?;
    if let Some(control) = output.control() {
        let mut canonical = BudgetedVec::new(control.memory());
        uqa_core::write_jsonb_equality_key(value, &mut canonical, control.cancellation())
            .map_err(resource_error)?;
        encode_bytes(&canonical, output)
    } else {
        let canonical = uqa_core::jsonb_equality_key(value)
            .ok_or_else(|| ExecError::Other("stored JSONB value is not valid JSON".into()))?;
        encode_bytes(&canonical, output)
    }
}

pub(super) fn resource_error(error: impl std::error::Error + Send + Sync + 'static) -> ExecError {
    ExecError::SQL(crate::storage_errors::storage_error(
        "encode canonical equality key",
        &uqa_storage::StorageBackendError::backend("canonical key", error),
    ))
}

/// Rust's integer and f64 Display representations fit this stack buffer, including the fixed notation for subnormal values. Formatting cannot allocate a temporary String.
pub(super) struct NumberText {
    bytes: [u8; 512],
    len: usize,
}

impl NumberText {
    pub(super) fn new(value: impl std::fmt::Display) -> ExecResult<Self> {
        use std::fmt::Write;
        let mut output = Self {
            bytes: [0; 512],
            len: 0,
        };
        write!(&mut output, "{value}").map_err(|_| {
            ExecError::Other("numeric equality text exceeds its fixed buffer".into())
        })?;
        Ok(output)
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub(super) fn as_str(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).expect("numeric Display writes UTF-8")
    }
}

impl std::fmt::Write for NumberText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(std::fmt::Error)?;
        let destination = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        destination.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}
