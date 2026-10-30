//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed mutation bodies enter the same complete command constructor without an intermediate unowned copy.

use super::{CommandPlan, MutableCommandPlan, SQLError};
use crate::plan::copy::Copying;
use uqa_core::memory::ProductionControl;

macro_rules! mutation_constructor {
    ($plan:ty, $variant:ident, $method:ident) => {
        impl $plan {
            pub fn copy_for_rewrite(
                &self,
                control: &ProductionControl<'_>,
            ) -> Result<MutableCommandPlan, SQLError> {
                control.check()?;
                let Some(mut copying) = Copying::for_rewrite(*control) else {
                    return Ok(MutableCommandPlan::from_rewrite_parts(
                        CommandPlan::$variant(Box::new(self.clone())),
                        None,
                    ));
                };
                let command = copying.$method(self)?;
                control.check()?;
                Ok(MutableCommandPlan::from_rewrite_parts(
                    command,
                    Some(copying.into_rewrite_memory()),
                ))
            }
        }
    };
}

mutation_constructor!(crate::plan::InsertPlan, Insert, insert_command);
mutation_constructor!(crate::plan::UpdatePlan, Update, update_command);
mutation_constructor!(crate::plan::DeletePlan, Delete, delete_command);
mutation_constructor!(crate::plan::MergePlan, Merge, merge_command);
