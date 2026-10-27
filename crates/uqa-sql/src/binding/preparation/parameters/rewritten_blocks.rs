//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewritten query blocks keep their analyzed expression addresses until installation in the executable tree.

use super::{BTreeMap, NonNull, ParameterTypes};
use crate::plan::{QueryBlockPlan, QueryPlan, RelationalPlan};

pub(super) type RewrittenBlocks = BTreeMap<NonNull<QueryBlockPlan>, Box<QueryBlockPlan>>;

pub(super) fn install_rewritten_block(blocks: &mut RewrittenBlocks, query: &mut QueryPlan) {
    if let RelationalPlan::QueryBlock(block) = &mut query.root {
        if let Some(rewritten) = blocks.remove(&NonNull::from(block.as_ref())) {
            *block = rewritten;
        }
    }
}

impl ParameterTypes<'_> {
    pub(in crate::binding::preparation) fn take_rewritten_block(
        &mut self,
        original: &QueryBlockPlan,
    ) -> Option<Box<QueryBlockPlan>> {
        self.rewritten_blocks.remove(&NonNull::from(original))
    }

    pub(in crate::binding::preparation) fn retain_rewritten_block(
        &mut self,
        original: &QueryBlockPlan,
        rewritten: Box<QueryBlockPlan>,
    ) {
        self.rewritten_blocks
            .insert(NonNull::from(original), rewritten);
    }
}
