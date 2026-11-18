//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{construct, retain, CommandPlan, Copying, Prefix, Result, Source};
use crate::plan::{copy::lowering::query::block::Inline, AggregateClassifier};

impl Copying<'_> {
    pub(in crate::plan::copy::lowering) fn lower_mutation(
        &mut self,
        source: Source<'_>,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<CommandPlan> {
        let mut scope = self.enter_plan_node()?;
        let mut input = Inline {
            aggregates,
            arena: Some(scope.query_arena()),
        };
        let result = crate::catalog::work::inline(retain(
            scope.control,
            construct(source, &mut scope, &mut input),
        )?)
        .map(Prefix::take);
        result
    }
}
