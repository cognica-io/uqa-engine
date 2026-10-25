//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parent-owned descriptors keep their catalog context and stable addresses.

use super::{boundary, Session, UQAType};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::memory::{MemoryReservation, OwnedMap, ProductionControl};
use uqa_sql::ColumnType;

pub(crate) struct TypeViews(Mutex<Views>);

struct Views {
    values: OwnedMap<u64, Box<UQAType>>,
    memory: Option<MemoryReservation>,
}

impl TypeViews {
    pub(crate) fn new(control: &ProductionControl<'_>) -> Self {
        Self(Mutex::new(Views {
            values: OwnedMap::new(),
            memory: control.empty_reservation(),
        }))
    }

    pub(crate) fn get(
        &self,
        key: u64,
        ty: &ColumnType,
        session: Option<&Arc<Session>>,
    ) -> boundary::Result<*const UQAType> {
        let control =
            session.map_or_else(ProductionControl::uncontrolled, |session| session.control());
        control.check()?;
        let mut views = self.0.lock();
        if let Some(view) = views.values.get(&key) {
            return Ok(std::ptr::from_ref(&**view));
        }
        let node = control.reserve(OwnedMap::<u64, Box<UQAType>>::entry_bytes())?;
        let view = UQAType::new(ty, session)?;
        let pointer = std::ptr::from_ref(&*view);
        views.values.insert(key, view);
        views.memory = control.combine(views.memory.take(), node);
        Ok(pointer)
    }
}
