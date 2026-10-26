//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{query::CteScope, RowSchema};
use uqa_sql::{
    plan::{
        source_projection::{ColumnPrune, QualifierFilters},
        QueryBlockPlan,
    },
    SQLError,
};

struct Decisions {
    remove_all: bool,
    remove_right: bool,
}

impl SourcePlanning<()> for Decisions {
    fn source_is_empty(&self, _source: &SourcePlan) -> bool {
        self.remove_all
    }
    fn join_input_is_empty(&self, _join: &SourcePlan, right: bool) -> bool {
        right && self.remove_right
    }
    fn column_prune_with_filter(
        &self,
        _: &QueryBlockPlan,
        _: &SourcePlan,
        _: Option<&ScalarExpr>,
        _: &CteScope<()>,
        _: Option<&RowSchema>,
    ) -> Result<Option<ColumnPrune>, SQLError> {
        unreachable!("initialization uses the supplied plan")
    }
    fn column_prune(
        &self,
        _: &QueryBlockPlan,
        _: &SourcePlan,
        _: &CteScope<()>,
        _: Option<&RowSchema>,
    ) -> Result<Option<ColumnPrune>, SQLError> {
        unreachable!("initialization uses the supplied plan")
    }
    fn qualifier_filters(
        &self,
        _: &QueryBlockPlan,
        _: &SourcePlan,
        _: &CteScope<()>,
    ) -> Result<Option<QualifierFilters>, SQLError> {
        unreachable!("initialization uses the supplied plan")
    }
    fn residual_filter(
        &self,
        _: &QueryBlockPlan,
        _: &SourcePlan,
        _: Option<&QualifierFilters>,
        _: &CteScope<()>,
    ) -> Result<Option<ScalarExpr>, SQLError> {
        unreachable!("initialization uses the supplied plan")
    }
    fn propagated_join_filters(
        &self,
        _: &QualifierFilters,
        _: &SourcePlan,
        _: &SourcePlan,
        _: Option<&ScalarExpr>,
    ) -> Option<QualifierFilters> {
        unreachable!("initialization uses the supplied plan")
    }
}

#[test]
fn surviving_source_arguments_keep_order_without_initializing_removed_inputs() {
    let plan = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile(
            "SELECT 1 FROM live_fn(abs(1)) l LEFT JOIN kept_fn(lower('Y')) r ON false",
        )
        .unwrap()
        .remove(0),
    );
    let uqa_sql::plan::UnifiedPlan::Query(query) = plan else {
        panic!("query")
    };
    let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let source = block.from.as_ref().unwrap();
    for (remove_all, remove_right, expected) in [
        (false, false, vec!["abs", "lower"]),
        (false, true, vec!["abs"]),
        (true, false, vec![]),
    ] {
        let mut expressions = Vec::with_capacity(3);
        let allocations = allocation_counter::measure(|| {
            visit(
                &Decisions {
                    remove_all,
                    remove_right,
                },
                source,
                &mut |expression| {
                    expressions.push(expression);
                    Ok(())
                },
            )
            .unwrap();
        });
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        let calls = expressions
            .iter()
            .filter_map(|expr| match expr {
                ScalarExpr::Func { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls, expected);
        if !remove_all {
            assert!(matches!(
                expressions.last(),
                Some(ScalarExpr::Literal(uqa_core::Value::Bool(false)))
            ));
        }
    }
}
