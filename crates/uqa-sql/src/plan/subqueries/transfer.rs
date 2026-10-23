//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move an expression's exclusively owned query children into another arena.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    plan::{QueryBlockPlan, QueryPlan},
    ScalarExpr,
};

/// Transfer each referenced query once and compact the source indices. The expression must already be detached from the source block. Shared or invalid references decline atomically; nested query arenas are never remapped. The caller owns the legality of changing evaluation scope.
pub fn transfer_expression(
    expression: &mut ScalarExpr,
    source: &mut QueryBlockPlan,
    destination: &mut Vec<QueryPlan>,
) -> bool {
    let mut selected = BTreeSet::new();
    crate::semantics::collect_subquery_ids(expression, &mut selected);
    if selected.is_empty() {
        return true;
    }
    let width = source.subqueries.len();
    if selected.iter().any(|id| *id >= width) {
        return false;
    }
    let mut exclusive = true;
    super::visit_block(source, &mut |node| {
        if let Some(id) = super::subquery_id(node) {
            exclusive &= *id < width && !selected.contains(id);
        }
    });
    if !exclusive {
        return false;
    }
    let moved: BTreeMap<_, _> = selected
        .iter()
        .enumerate()
        .map(|(position, id)| (*id, destination.len() + position))
        .collect();
    let mut retained = BTreeMap::new();
    for (id, query) in std::mem::take(&mut source.subqueries)
        .into_iter()
        .enumerate()
    {
        if selected.contains(&id) {
            destination.push(query);
        } else {
            retained.insert(id, source.subqueries.len());
            source.subqueries.push(query);
        }
    }
    crate::plan::rewrite_scalar_expression(expression, &mut |node| {
        if let Some(id) = super::subquery_id(node) {
            *id = moved[id];
        }
    });
    super::visit_block(source, &mut |node| {
        if let Some(id) = super::subquery_id(node) {
            *id = retained[id];
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{RelationalPlan, UnifiedPlan};

    fn block(sql: &str) -> Box<QueryBlockPlan> {
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        block
    }

    #[test]
    fn transfer_preserves_query_ownership_and_nested_arena_indices() {
        let mut source =
            block("SELECT (SELECT 11),(SELECT 22),(SELECT 33) WHERE (SELECT (SELECT 44))=44");
        let mut expression = source.r#where.take().unwrap();
        let mut destination = block("SELECT (SELECT 55)").subqueries;
        let nested_before = serde_json::to_value(&source.subqueries[3]).unwrap();
        assert!(transfer_expression(
            &mut expression,
            &mut source,
            &mut destination
        ));
        assert_eq!(source.subqueries.len(), 3);
        assert_eq!(destination.len(), 2);
        assert_eq!(
            serde_json::to_value(&destination[1]).unwrap(),
            nested_before
        );
        let mut ids = BTreeSet::new();
        crate::semantics::collect_subquery_ids(&expression, &mut ids);
        assert_eq!(ids, BTreeSet::from([1]));
        let mut expression = source.projections.remove(1).expr;
        assert!(transfer_expression(
            &mut expression,
            &mut source,
            &mut destination
        ));
        assert_eq!(expression, ScalarExpr::ScalarSubquery(2));
        assert_eq!(source.projections[1].expr, ScalarExpr::ScalarSubquery(1));
        assert_eq!(source.subqueries.len(), 2);
    }

    #[test]
    fn shared_and_invalid_arena_transfers_leave_all_inputs_unchanged() {
        for id in [0, 9] {
            let mut source = block("SELECT (SELECT 11)");
            let mut destination = block("SELECT (SELECT 22)").subqueries;
            let mut expression = ScalarExpr::ScalarSubquery(id);
            let before = serde_json::to_value((&source, &destination, &expression)).unwrap();
            assert!(!transfer_expression(
                &mut expression,
                &mut source,
                &mut destination
            ));
            assert_eq!(
                serde_json::to_value((&source, &destination, &expression)).unwrap(),
                before
            );
        }
    }

    #[test]
    fn transferring_wide_query_payload_does_not_copy_it() {
        let mut source = block("SELECT (SELECT 'x')");
        let RelationalPlan::QueryBlock(nested) = &mut source.subqueries[0].root else {
            panic!("nested block")
        };
        nested.projections[0].expr =
            ScalarExpr::Literal(uqa_core::Value::Str("x".repeat(256 * 1024)));
        let mut expression = source.projections.pop().unwrap().expr;
        let mut destination = Vec::new();
        let allocation = allocation_counter::measure(|| {
            assert!(transfer_expression(
                &mut expression,
                &mut source,
                &mut destination
            ));
        });
        assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
        let RelationalPlan::QueryBlock(nested) = &destination[0].root else {
            panic!("nested block")
        };
        assert!(
            matches!(&nested.projections[0].expr, ScalarExpr::Literal(uqa_core::Value::Str(text)) if text.len() == 256 * 1024)
        );
    }
}
