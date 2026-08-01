//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Byte-bounded SQL set operations.

use super::{Batch, ExecResult, PhysicalOperator, SetOpKind};

/// Byte-bounded compatibility wrapper for SQL set operations.
///
/// All forms other than `UNION ALL` externally sort and merge their inputs;
/// `UNION ALL` streams both children. Construction is fallible because input
/// widths must agree.
pub struct SetOperation<'a> {
    inner: crate::set_operation::ExternalSetOperation<'a>,
}

impl<'a> SetOperation<'a> {
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
    ) -> ExecResult<Self> {
        Self::new_with_work_mem(left, right, kind, all, 64 * 1024 * 1024)
    }

    pub fn new_with_work_mem(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: SetOpKind,
        all: bool,
        work_mem_bytes: usize,
    ) -> ExecResult<Self> {
        Ok(Self {
            inner: crate::set_operation::ExternalSetOperation::new(
                left,
                right,
                kind,
                all,
                work_mem_bytes,
            )?,
        })
    }
}

impl PhysicalOperator for SetOperation<'_> {
    fn schema(&self) -> &[String] {
        self.inner.schema()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.inner.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.inner.next()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.inner.close()
    }
}

// -------------------------------------------------------------------------
// Hash aggregate
// -------------------------------------------------------------------------
