//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewritten query blocks keep their analyzed expression addresses until installation in the executable tree.

use super::{AnalysisMap, NonNull, ParameterTypes, SQLError};
use crate::plan::{MutableQueryPlan, QueryBlockPlan, QueryPlan, RelationalPlan};

pub(super) type RewrittenBlocks = AnalysisMap<NonNull<QueryBlockPlan>, MutableQueryPlan>;

pub(super) fn install_rewritten_block(blocks: &mut RewrittenBlocks, query: &mut QueryPlan) {
    if let RelationalPlan::QueryBlock(block) = &mut query.root {
        if let Some(rewritten) = blocks.remove(&NonNull::from(block.as_ref())) {
            let rewritten = rewritten
                .finish(&uqa_core::memory::ProductionControl::uncontrolled())
                .expect("ordinary rewritten block")
                .into_uncontrolled()
                .expect("ordinary rewritten block has no reservation");
            let RelationalPlan::QueryBlock(rewritten) = rewritten.root else {
                unreachable!("retained rewritten query block");
            };
            *block = rewritten;
        }
    }
}

impl ParameterTypes<'_> {
    pub(in crate::binding::preparation) fn take_rewritten_block(
        &mut self,
        original: &QueryBlockPlan,
    ) -> Option<MutableQueryPlan> {
        self.rewritten_blocks.remove(&NonNull::from(original))
    }

    pub(in crate::binding::preparation) fn retain_rewritten_block(
        &mut self,
        original: &QueryBlockPlan,
        rewritten: MutableQueryPlan,
    ) -> Result<(), SQLError> {
        self.rewritten_blocks
            .insert(NonNull::from(original), rewritten)?;
        Ok(())
    }
}
