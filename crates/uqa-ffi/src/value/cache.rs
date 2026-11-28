//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable storage for derived C views, allocated on the first successful observation that needs it.

use super::{arrays, boundary, ranges, records, UQAValue};
use crate::{abi::UQAArrayDimension, types::TypeViews};
use parking_lot::Mutex;
use std::sync::OnceLock;
use uqa_core::{
    memory::{MemoryReservation, Produced},
    Value,
};
use uqa_sql::params::SQLTensor;

pub(super) struct UQAValueExtras {
    pub(super) decoded: OnceLock<Produced<Value>>,
    pub(super) numeric_text: OnceLock<Produced<String>>,
    pub(super) uuid_bytes: OnceLock<[u8; 16]>,
    pub(super) vector_data: OnceLock<Produced<Vec<f32>>>,
    pub(super) tensor_data: OnceLock<Produced<SQLTensor>>,
    pub(super) array_shape: OnceLock<Produced<Vec<UQAArrayDimension>>>,
    pub(super) array_children: Mutex<arrays::Children>,
    pub(super) record_views: Mutex<records::RecordViews>,
    pub(super) ranges: OnceLock<Box<ranges::RangeViews>>,
    pub(super) types: TypeViews,
    _memory: Option<MemoryReservation>,
}

impl UQAValue {
    fn allocate_extras(&self) -> boundary::Result<Box<UQAValueExtras>> {
        let control = self.control();
        let memory = control.reserve(size_of::<UQAValueExtras>())?;
        boundary::boxed(UQAValueExtras {
            decoded: OnceLock::new(),
            numeric_text: OnceLock::new(),
            uuid_bytes: OnceLock::new(),
            vector_data: OnceLock::new(),
            tensor_data: OnceLock::new(),
            array_shape: OnceLock::new(),
            array_children: Mutex::new(arrays::Children::new(&control)),
            record_views: Mutex::new(records::RecordViews::new(&control)),
            ranges: OnceLock::new(),
            types: TypeViews::new(&control),
            _memory: memory,
        })
    }

    pub(super) fn cached<T>(
        &self,
        slot: fn(&UQAValueExtras) -> &OnceLock<T>,
        build: impl FnOnce() -> boundary::Result<T>,
    ) -> boundary::Result<&T> {
        if let Some(value) = self.extras.get().and_then(|extras| slot(extras).get()) {
            return Ok(value);
        }
        let value = build()?;
        let extras = if let Some(extras) = self.extras.get() {
            extras
        } else {
            let candidate = self.allocate_extras()?;
            self.extras.get_or_init(|| candidate)
        };
        Ok(slot(extras).get_or_init(|| value))
    }

    pub(super) fn with_views<R>(
        &self,
        observe: impl Fn(&UQAValueExtras) -> boundary::Result<R>,
    ) -> boundary::Result<R> {
        if let Some(extras) = self.extras.get() {
            return observe(extras);
        }
        let candidate = self.allocate_extras()?;
        let view = observe(&candidate)?;
        if self.extras.set(candidate).is_ok() {
            return Ok(view);
        }
        // Concurrent readers must all receive pointers into the published owner.
        observe(self.extras.get().expect("a reader published the views"))
    }
}
