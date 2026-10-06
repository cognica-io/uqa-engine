//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-owned scalar-subquery arena compaction after optimizer rewrites.

pub(super) use uqa_sql::plan::subqueries::{prune_command, prune_query};

/// Retain dead branches for static result typing without evaluating their child plans.
pub(super) fn live_slots<'a>(
    expressions: impl IntoIterator<Item = &'a uqa_sql::ScalarExpr>,
) -> Result<std::collections::BTreeSet<usize>, uqa_sql::SQLError> {
    let mut slots = std::collections::BTreeSet::new();
    for expression in expressions {
        uqa_sql::catalog::security::builtin_routines::initialization::visit(
            expression,
            &mut |node| {
                match node {
                    uqa_sql::ScalarExpr::ScalarSubquery(index)
                    | uqa_sql::ScalarExpr::Exists {
                        subquery: index, ..
                    }
                    | uqa_sql::ScalarExpr::InSubquery {
                        subquery: index, ..
                    } => {
                        slots.insert(*index);
                    }
                    _ => {}
                }
                Ok(())
            },
        )?;
    }
    Ok(slots)
}

/// Plan only subquery slots reached by a surviving expression. Unreached slots retain their analyzed result types.
pub(super) fn optimize_live(
    queries: &mut [super::QueryPlan],
    slots: &std::collections::BTreeSet<usize>,
    config: &super::OptimizerConfig,
    aggregates: &dyn super::AggregateClassifier,
) -> Result<(), uqa_sql::SQLError> {
    for (index, query) in queries.iter_mut().enumerate() {
        if slots.contains(&index) {
            super::optimize_query(query, config, aggregates)?;
        }
    }
    Ok(())
}
