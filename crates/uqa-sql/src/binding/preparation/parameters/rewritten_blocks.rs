//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewritten query blocks keep their analyzed expression addresses until installation in the executable tree.

use super::{AnalysisIdentity, AnalysisMap, ParameterTypes, SQLError};
use crate::plan::{QueryBlockPlan, QueryPlan, QueryReplacement, RelationalPlan};

pub(super) type RewrittenBlocks = AnalysisMap<AnalysisIdentity<QueryBlockPlan>, QueryReplacement>;

pub(super) fn install_rewritten_block(
    blocks: &mut RewrittenBlocks,
    query: &mut QueryPlan,
) -> Result<(), SQLError> {
    if let RelationalPlan::QueryBlock(block) = &query.root {
        if let Some(rewritten) = blocks.remove(&AnalysisIdentity::from(block.as_ref())) {
            assert!(matches!(rewritten.root, RelationalPlan::QueryBlock(_)));
            rewritten.install_uncontrolled(query)?;
        }
    }
    Ok(())
}

impl<I: super::LiteralInput> ParameterTypes<'_, I> {
    pub(in crate::binding::preparation) fn take_rewritten_block(
        &mut self,
        original: &QueryBlockPlan,
    ) -> Option<QueryReplacement> {
        self.rewritten_blocks
            .remove(&AnalysisIdentity::from(original))
    }

    pub(in crate::binding::preparation) fn retain_rewritten_block(
        &mut self,
        original: &QueryBlockPlan,
        rewritten: impl Into<QueryReplacement>,
    ) -> Result<(), SQLError> {
        self.rewritten_blocks
            .insert(AnalysisIdentity::from(original), rewritten.into())?;
        Ok(())
    }
}
