//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

#[path = "fixture.rs"]
mod fixture;

fn expression(sql: &str) -> ScalarExpr {
    let statement = uqa_sql::compile(sql).unwrap().remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query required");
    };
    let RelationalPlan::QueryBlock(mut block) = query.root else {
        panic!("query block required");
    };
    block.projections.remove(0).expr
}

fn projection(expression: &ScalarExpr) -> (bool, SourceProjection) {
    let qualifiers = ["items".to_string()];
    let scope = PruneScope {
        qualifiers: &qualifiers,
        metadata_qualifier: Some("items"),
        legacy_doc_id: true,
        legacy_score: true,
    };
    let mut prune = ColumnPrune::from([("items".to_string(), SourceProjection::default())]);
    let mut valid = true;
    collect_expr_prune_columns(expression, scope, &mut prune, &mut valid);
    (valid, prune.remove("items").unwrap())
}

#[test]
fn row_count_star_has_no_document_payload_dependency() {
    for sql in ["SELECT count(*)", "SELECT COUNT(*)", "SELECT count(*) + 1"] {
        let (valid, projection) = projection(&expression(sql));
        assert!(valid, "{sql}");
        assert!(projection.metadata().is_empty());
        assert_eq!(projection.explicit_columns(), Some(BTreeSet::new()));
    }
}

#[test]
fn count_star_keeps_filter_and_ordering_dependencies() {
    let mut count = expression("SELECT count(*) FILTER (WHERE quantity > 0)");
    let ordered = expression("SELECT count(price ORDER BY price DESC)");
    let ScalarExpr::Func { order_by, .. } = ordered else {
        panic!("aggregate required");
    };
    if let ScalarExpr::Func {
        order_by: target, ..
    } = &mut count
    {
        *target = order_by;
    }
    let (valid, projection) = projection(&count);
    assert!(valid);
    assert_eq!(
        projection.explicit_columns(),
        Some(BTreeSet::from(["price".into(), "quantity".into()]))
    );
}

#[test]
fn count_column_and_composite_counts_keep_their_input_values() {
    let (valid, selected) = projection(&expression("SELECT count(price)"));
    assert!(valid);
    assert_eq!(
        selected.explicit_columns(),
        Some(BTreeSet::from(["price".into()]))
    );
    let (valid, selected) = projection(&expression("SELECT count(DISTINCT items)"));
    assert!(valid);
    assert_eq!(selected.explicit_columns(), None);
    for sql in ["SELECT count(items.*)", "SELECT *"] {
        assert!(!projection(&expression(sql)).0, "{sql}");
    }
}

#[test]
fn other_function_stars_and_invalid_count_arities_remain_conservative() {
    let mut count = expression("SELECT count(*)");
    if let ScalarExpr::Func { name, .. } = &mut count {
        *name = "custom".into();
    }
    assert!(!projection(&count).0);
    if let ScalarExpr::Func { name, args, .. } = &mut count {
        *name = "count".into();
        args.push(ScalarExpr::Column("price".into()));
    }
    assert!(!projection(&count).0);
}

#[test]
fn accelerated_count_prunes_only_consumed_filter_columns() {
    let catalog = fixture::Fixture::new();
    let statement = uqa_sql::compile("SELECT count(*) FROM items WHERE fts_match(body, 'common')")
        .unwrap()
        .remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query required");
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block required");
    };
    let source = block.from.as_ref().unwrap();
    let mut ordinary = column_prune_for_stmt(catalog.context(), &block, source)
        .unwrap()
        .unwrap();
    assert_eq!(
        ordinary.remove("items").unwrap().explicit_columns(),
        Some(BTreeSet::from(["body".into()]))
    );
    let mut accelerated =
        column_prune_for_stmt_with_filter(catalog.context(), &block, source, None)
            .unwrap()
            .unwrap();
    assert_eq!(
        accelerated.remove("items").unwrap().explicit_columns(),
        Some(BTreeSet::new())
    );
    let residual = expression("SELECT quantity > 0");
    let mut filtered =
        column_prune_for_stmt_with_filter(catalog.context(), &block, source, Some(&residual))
            .unwrap()
            .unwrap();
    assert_eq!(
        filtered.remove("items").unwrap().explicit_columns(),
        Some(BTreeSet::from(["quantity".into()]))
    );
}
