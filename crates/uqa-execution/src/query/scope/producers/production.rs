//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CteProducers, ProducerList};
use uqa_core::memory::ProductionControl;
use uqa_sql::SQLError;

impl CteProducers {
    pub(in crate::query::scope) fn with_control(
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let list = super::super::production::shared(
            ProducerList(parking_lot::Mutex::new(Some(
                uqa_core::memory::OwnedMap::new(),
            ))),
            control,
        )?;
        Ok(Self::Owned(list))
    }
}
