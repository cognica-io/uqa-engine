//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar-query compaction keeps the existing remapping algorithm and retires child owners with their inputs.

use super::{memory::RewriteMemory, MutableCommandPlan, MutableQueryPlan};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

impl MutableQueryPlan {
    /// Compact the local scalar-query arena through SQL's existing namespace-aware remapping. Unreachable child payloads release before their constructor records, while the original arena capacity remains owned by the parent.
    pub fn prune_subqueries(mut self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let mut fields = self.memory.as_mut().map(|memory| {
            let root = memory.field_mut("root").as_mut().expect("query root");
            let block = match root {
                RewriteMemory::Fields { fields, .. } => {
                    fields.iter().any(|field| field.name == "block")
                }
                RewriteMemory::Scalar(_) => unreachable!("query root constructor"),
            };
            let root = if block {
                root.field_mut("block").as_mut().expect("query block")
            } else {
                root
            };
            match root
                .field_mut("subqueries")
                .as_mut()
                .expect("scalar query arena")
            {
                RewriteMemory::Fields { fields, .. } => fields,
                RewriteMemory::Scalar(_) => unreachable!("query arena constructor"),
            }
        });
        super::super::subqueries::prune_query_with_retention(
            &mut self.query,
            *control,
            |arena, keep| {
                let mut retained = 0;
                for index in 0..arena.len() {
                    if keep(index) {
                        arena.swap(retained, index);
                        if let Some(fields) = &mut fields {
                            fields.swap(retained, index);
                        }
                        retained += 1;
                    }
                }
                arena.truncate(retained);
                if let Some(fields) = fields {
                    fields.truncate(retained);
                }
            },
        )?;
        Ok(self)
    }
}

impl MutableCommandPlan {
    /// Compact a mutation's scalar-query arena using its existing source-then-expression namespace remapping. Command CTEs and retained source queries keep their independent owners.
    pub fn prune_subqueries(mut self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        if self.command.scalar_subqueries().is_empty() {
            return Ok(self);
        }
        let mut fields = self.memory.as_mut().map(|memory| {
            let arena = memory
                .field_mut("value0")
                .as_mut()
                .expect("mutation body")
                .field_mut("subqueries")
                .as_mut()
                .expect("mutation arena");
            match arena {
                RewriteMemory::Fields { fields, .. } => fields,
                RewriteMemory::Scalar(_) => unreachable!("query arena constructor"),
            }
        });
        super::super::subqueries::prune_command_with_retention(
            &mut self.command,
            *control,
            |arena, keep| {
                let mut retained = 0;
                for index in 0..arena.len() {
                    if keep(index) {
                        arena.swap(retained, index);
                        if let Some(fields) = &mut fields {
                            fields.swap(retained, index);
                        }
                        retained += 1;
                    }
                }
                arena.truncate(retained);
                if let Some(fields) = fields {
                    fields.truncate(retained);
                }
            },
        )?;
        Ok(self)
    }
}
