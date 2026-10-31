//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzed grouping selection retires discarded sets with their constructor owners.

use super::{memory::RewriteMemory, MutableQueryPlan, RelationalPlan};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

impl MutableQueryPlan {
    pub fn apply_grouping(mut self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let Some(grouping) = self.query.analyzed_grouping.take() else {
            return Ok(self);
        };
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "analyzed grouping belongs to a query block".into(),
            ));
        };
        let mut fields = self.memory.as_mut().map(|memory| {
            let block = memory
                .field_mut("root")
                .as_mut()
                .expect("query root")
                .field_mut("block")
                .as_mut()
                .expect("query block");
            match block
                .field_mut("grouping_sets")
                .as_mut()
                .expect("grouping sets")
            {
                RewriteMemory::Fields { fields, .. } => fields,
                RewriteMemory::Scalar(_) => unreachable!("grouping-set vector constructor"),
            }
        });
        grouping.apply_with_retention(block, |sets, keep| {
            let mut retained = 0;
            for index in 0..sets.len() {
                if keep(index) {
                    sets.swap(retained, index);
                    if let Some(fields) = &mut fields {
                        fields.swap(retained, index);
                    }
                    retained += 1;
                }
            }
            sets.truncate(retained);
            if let Some(fields) = fields {
                fields.truncate(retained);
            }
        })?;
        if let Some(memory) = &mut self.memory {
            // The consumed selection vector is already destroyed by its semantic owner.
            drop(memory.field_mut("analyzed_grouping").take());
        }
        Ok(self)
    }

    /// Captured source layouts describe the pre-rewrite topology. Their immutable metadata retains its existing shared owner.
    pub fn discard_source_analysis(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        self.query.analyzed_source = None;
        Ok(self)
    }
}
