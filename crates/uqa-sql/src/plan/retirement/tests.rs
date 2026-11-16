//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ir::ConstantValueSource, SQLError};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{memory::MemoryBudget, Value};

/// Observe final native payload release independently of the retirement state machine.
#[derive(Debug)]
pub(in crate::plan) struct ScalarSource {
    pub(in crate::plan) native: MemoryBudget,
    pub(in crate::plan) minimum: Arc<AtomicUsize>,
    pub(in crate::plan) retired: Arc<AtomicUsize>,
}

impl ConstantValueSource for ScalarSource {
    fn read(&self, _: u64) -> Result<Value, SQLError> {
        panic!("retirement must not read evaluated constants")
    }
}

impl Drop for ScalarSource {
    fn drop(&mut self) {
        assert!(
            self.native.used() >= self.minimum.load(Ordering::Relaxed),
            "payloads must retire before their constructor reservations"
        );
        self.retired.fetch_add(1, Ordering::Relaxed);
    }
}
