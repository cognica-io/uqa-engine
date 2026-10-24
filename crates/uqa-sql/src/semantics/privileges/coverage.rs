//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural coverage of one successful complete SELECT privilege check.

use super::{context::PrivilegeScope, ensure_select_privileges_for_query_block};
use crate::{
    plan::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan},
    SQLError,
};

/// Borrows only syntax visited by a successful complete check. The consumer must independently preserve the immutable catalog, namespace, CTE definitions, outer lineage and complete authorization subject. Structural coverage alone never authorizes a changed execution scope.
#[derive(Clone, Copy)]
pub struct SelectSourceCoverage<'a> {
    source: &'a SourcePlan,
    block: Option<&'a QueryBlockPlan>,
}

impl<'a> SelectSourceCoverage<'a> {
    pub fn matches_source(&self, source: &SourcePlan) -> bool {
        std::ptr::eq(self.source, source)
    }

    /// A changed projection must be checked even when it uses the same source.
    pub fn covers_query(&self, block: &QueryBlockPlan, source: &SourcePlan) -> bool {
        self.matches_source(source)
            && self
                .block
                .is_some_and(|checked| std::ptr::eq(checked, block))
    }

    pub fn join_child(&self, right: bool) -> Option<Self> {
        let SourcePlan::Join {
            left, right: rhs, ..
        } = self.source
        else {
            return None;
        };
        Some(Self {
            source: if right { rhs } else { left },
            block: None,
        })
    }

    /// Project coverage onto an original direct derived block. CTE and set-operation execution keep their independent checks.
    pub fn query_source(&self, block: &QueryBlockPlan) -> Option<Self> {
        let SourcePlan::Subquery { body, .. } = self.source else {
            return None;
        };
        if !body.ctes.is_empty() {
            return None;
        }
        let RelationalPlan::QueryBlock(checked) = &body.root else {
            return None;
        };
        if !std::ptr::eq(checked.as_ref(), block) {
            return None;
        }
        Some(Self {
            source: checked.from.as_ref()?,
            block: Some(checked),
        })
    }
}

/// Complete the original ordered lineage and table/column checks before issuing any coverage. Failed or partial expression checks cannot construct this value.
pub fn check_query_source<'a>(
    block: &'a QueryBlockPlan,
    source: &'a SourcePlan,
    scope: &PrivilegeScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<SelectSourceCoverage<'a>, SQLError> {
    ensure_select_privileges_for_query_block(block, source, scope)?;
    Ok(SelectSourceCoverage {
        source,
        block: Some(block),
    })
}

#[cfg(test)]
mod tests;
