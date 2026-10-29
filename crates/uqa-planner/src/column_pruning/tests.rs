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
        source_schema: None,
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

#[test]
fn unused_window_definition_keeps_its_required_source_columns() {
    let catalog = fixture::Fixture::new();
    let statement =
        uqa_sql::compile("SELECT 1 FROM items WINDOW unused AS (ORDER BY sum(quantity))")
            .unwrap()
            .remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query required")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block required")
    };
    let mut projection =
        column_prune_for_stmt(catalog.context(), &block, block.from.as_ref().unwrap())
            .unwrap()
            .unwrap();
    assert_eq!(
        projection.remove("items").unwrap().explicit_columns(),
        Some(BTreeSet::from(["quantity".into()]))
    );
}

#[test]
fn conservative_projection_retains_explicit_document_demands() {
    let catalog = fixture::Fixture::new();
    for sql in [
        "SELECT *, price FROM items",
        "SELECT price + random() FROM items",
        "SELECT price FROM items WHERE EXISTS (SELECT 1)",
    ] {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("query required")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block required")
        };
        let mut projection =
            column_prune_for_stmt(catalog.context(), &block, block.from.as_ref().unwrap())
                .unwrap()
                .unwrap();
        let wanted = projection.remove("items").unwrap();
        assert_eq!(
            wanted.requested_columns().collect::<Vec<_>>(),
            ["price"],
            "{sql}"
        );
        assert_eq!(wanted.explicit_columns(), None, "{sql}");
    }
}

#[test]
fn bound_columns_do_not_become_competing_open_document_fields() {
    let catalog = fixture::Fixture::new();
    let schema = uqa_sql::RowSchema::with_qualified_types(
        "items",
        vec!["price".into()],
        vec![Some(uqa_sql::ColumnType::Integer)],
    );
    let schema = uqa_sql::RowSchema::with_open_columns(&schema, Some("d"));
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(
        uqa_sql::compile("SELECT price, payload FROM items CROSS JOIN docs d")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query required")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block required")
    };
    let mut context = catalog.context();
    context.source_schema = Some(&schema);
    let projection = column_prune_for_stmt(context, &block, block.from.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert!(projection["items"].contains("price"));
    assert!(!projection["d"].contains("price"));
    assert!(projection["d"].contains("payload"));
}

#[test]
fn correlated_subqueries_keep_metadata_demands_in_the_owning_source() {
    let catalog = fixture::Fixture::new();
    for sql in [
        "SELECT (SELECT _meta.doc_id), (SELECT _meta.score) FROM items",
        "SELECT price FROM items WHERE EXISTS (SELECT 1 WHERE _meta.doc_id>0 AND _meta.score=0)",
        "SELECT price FROM items ORDER BY (SELECT (SELECT _meta.doc_id+_meta.score))",
        "SELECT sum(price) FILTER (WHERE 1 IN (SELECT _meta.doc_id+_meta.score)) FROM items",
        "SELECT price FROM items JOIN (VALUES (1)) marker(n) ON marker.n=(SELECT _meta.doc_id+_meta.score)",
        "SELECT price FROM items CROSS JOIN LATERAL (SELECT _meta.doc_id+_meta.score) derived",
    ] {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        let projection =
            column_prune_for_stmt(catalog.context(), &block, block.from.as_ref().unwrap())
                .unwrap()
                .unwrap();
        assert!(projection["items"].metadata().includes_doc_id(), "{sql}");
        assert!(projection["items"].metadata().includes_score(), "{sql}");
    }
}

#[test]
fn a_subquery_local_metadata_alias_does_not_request_outer_metadata() {
    let catalog = fixture::Fixture::new();
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile(
            "SELECT (SELECT _meta.doc_id + _meta.score FROM (VALUES (7,8)) _meta(doc_id,score)) FROM items",
        )
        .unwrap()
        .remove(0),
    ) else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.subqueries[0].analyzed_source = Some(
        std::sync::Arc::new(uqa_sql::plan::AnalyzedSource {
            schema: uqa_sql::RowSchema::with_qualified_types(
                "_meta",
                vec!["doc_id".into(), "score".into()],
                vec![Some(uqa_sql::ColumnType::Integer); 2],
            ),
            children: None,
        })
        .into(),
    );
    let projection = column_prune_for_stmt(catalog.context(), block, block.from.as_ref().unwrap())
        .unwrap()
        .unwrap();
    assert!(projection["items"].metadata().is_empty());
}
