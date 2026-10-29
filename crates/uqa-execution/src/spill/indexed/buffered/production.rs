//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained constants transfer an admitted record into their resident or atomic disk owner.

use super::{
    BudgetedVec, BufferedIndexedSpill, ExecResult, IndexedSpill, Produced, ProductionControl,
};
use crate::spill::format::encode_constant_record_with_control;
use uqa_core::Value;

impl BufferedIndexedSpill {
    pub(crate) fn push_constant_with_control(
        &mut self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> ExecResult<()> {
        super::super::check_publication(control)?;
        let record = encode_constant_record_with_control(value, control)?;
        if let Some(disk) = &mut self.disk {
            return disk.push_encoded_with_control(&record, control);
        }
        if self.rows.reserve(1).is_ok() {
            if let Ok(resident) = self.rows.budget().reserve(record.capacity()) {
                super::super::check_publication(control)?;
                // The encoder's lease covers construction. Admit the destination store before transferring the same allocation; the request lease remains live until publication under that store's allowance.
                let (record, request) = record.into_parts();
                self.rows
                    .push(record)
                    .map_err(|error| super::spill_error(error.to_string()))?;
                self.payload.absorb(resident);
                drop(request);
                return Ok(());
            }
        }
        self.spill_constant(record, control)
    }

    fn spill_constant(
        &mut self,
        record: Produced<Vec<u8>>,
        control: &ProductionControl<'_>,
    ) -> ExecResult<()> {
        super::super::check_publication(control)?;
        let mut disk = IndexedSpill::new(self.schema.clone())?;
        for previous in self.rows.iter() {
            disk.push_encoded_with_control(previous, control)?;
        }
        disk.push_encoded_with_control(&record, control)?;
        super::super::check_publication(control)?;
        // Only the complete private prefix plus incoming record can replace the resident owner. A failure above drops private files and leaves the published prefix available.
        let memory = self.rows.budget().clone();
        self.rows = BudgetedVec::new(&memory);
        self.payload = memory.empty_reservation();
        self.disk = Some(disk);
        Ok(())
    }
}
