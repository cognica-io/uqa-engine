//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Conjunct placement by the columns that each FROM relation exposes.

use super::*;

/// The lowered query block of one SELECT statement.
fn query_block_of(sql: &str) -> QueryBlockPlan {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = plan.root else {
        panic!("query block")
    };
    *block
}

/// The placed predicates and the residual filter of one query block, with the WHERE conjuncts they are derived from.
fn placement(
    sql: &str,
) -> (
    Vec<ScalarExpr>,
    Option<QualifierFilters>,
    Option<ScalarExpr>,
) {
    let fixture = Fixture::new();
    let block = query_block_of(sql);
    let from = block.from.as_ref().unwrap();
    let filters =
        qualifier_filters_for_stmt(fixture.context(), &block, from, fixture.scope()).unwrap();
    let residual = final_filter_after_qualifier_pushdown(
        fixture.context(),
        &block,
        from,
        filters.as_ref(),
        fixture.scope(),
    )
    .unwrap();
    let parts = flatten_and_filter_parts(block.r#where.as_ref().unwrap())
        .into_iter()
        .cloned()
        .collect();
    (parts, filters, residual)
}

fn placed(entries: &[(&str, ScalarExpr)]) -> Option<QualifierFilters> {
    let mut filters = QualifierFilters::new();
    for (qualifier, predicate) in entries {
        filters.push(qualifier, predicate.clone());
    }
    (!filters.is_empty()).then_some(filters)
}

#[test]
fn whole_row_references_of_values_lists_stay_in_the_residual() {
    let (parts, filters, residual) = placement(
        "SELECT * FROM (VALUES (1, 2), (3, 4)) v(x, y) WHERE v = ROW(3, 4) AND v IS NOT NULL AND (v.x > 1 OR v IS NULL) AND row_to_json(v)::text LIKE '%3%' AND x > 1",
    );
    assert_eq!(
        filters,
        placed(&[("v", qualify_unqualified_columns(&parts[4], "v"))])
    );
    assert_eq!(residual, combine_filter_parts(parts[..4].to_vec()));
}

#[test]
fn derived_and_function_relations_receive_only_their_columns() {
    let (parts, filters, residual) =
        placement("SELECT * FROM (SELECT 1 AS x, 2 AS y) s WHERE s = ROW(1, 2) AND x > 0");
    assert_eq!(
        filters,
        placed(&[("s", qualify_unqualified_columns(&parts[1], "s"))])
    );
    assert_eq!(residual, Some(parts[0].clone()));

    // A table function without column aliases does not determine its column list, and its own name is ambiguous with a whole-row value.
    let (parts, filters, residual) = placement("SELECT * FROM generate_series(1, 3) g WHERE g > 1");
    assert_eq!(filters, None);
    assert_eq!(residual, Some(parts[0].clone()));

    let (parts, filters, residual) =
        placement("SELECT * FROM generate_series(1, 3) g(n) WHERE g IS NOT NULL AND n > 1");
    assert_eq!(
        filters,
        placed(&[("g", qualify_unqualified_columns(&parts[1], "g"))])
    );
    assert_eq!(residual, Some(parts[0].clone()));

    // An unlisted name that is not a relation's name still reads a column of the only relation whose column list is not determined.
    let (parts, filters, residual) =
        placement("SELECT * FROM jsonb_each('{}') e WHERE value IS NOT NULL");
    assert_eq!(
        filters,
        placed(&[("e", qualify_unqualified_columns(&parts[0], "e"))])
    );
    assert_eq!(residual, None);

    // A name that the only relation does not list is an outer reference or a whole-row value of another scope.
    let (parts, filters, residual) =
        placement("SELECT * FROM (VALUES (1)) v(x) WHERE x > outer_value");
    assert_eq!(filters, None);
    assert_eq!(residual, Some(parts[0].clone()));
}

#[test]
fn column_free_retrieval_predicates_keep_the_only_named_relation() {
    let (parts, filters, residual) = placement(
        "SELECT * FROM filter_alias_source, (SELECT 1) WHERE graph_pagerank('gnet') AND id > 0",
    );
    assert_eq!(
        filters,
        placed(&[
            ("filter_alias_source", parts[0].clone()),
            (
                "filter_alias_source",
                qualify_unqualified_columns(&parts[1], "filter_alias_source")
            ),
        ])
    );
    assert_eq!(residual, None);

    let (parts, filters, residual) = placement(
        "SELECT * FROM filter_alias_source, (VALUES (1)) WHERE column1 > 0 AND graph_pagerank('gnet')",
    );
    assert_eq!(
        filters,
        placed(&[("filter_alias_source", parts[1].clone())])
    );
    assert_eq!(residual, Some(parts[0].clone()));
}

#[test]
fn aliased_and_nested_joins_expose_only_their_visible_relations() {
    // The alias hides the inner relations, so only the outer `v` lists `x`; conjuncts on the alias are placed for the alias but kept in the residual filter, which the alias makes unsafe to elide.
    let block = query_block_of(
        "SELECT * FROM (VALUES (1)) v(x) CROSS JOIN ((VALUES (2)) v(x) CROSS JOIN (VALUES (3)) b(y)) AS j WHERE v.x > 1 AND j.y > 2",
    );
    let from = block.from.as_ref().unwrap();
    let fixture = Fixture::new();
    let columns = SourceColumns::new(fixture.scope(), from, &from_qualifier_set(from)).unwrap();
    assert_eq!(
        columns
            .owners()
            .get("x")
            .map(|owner| owner.as_ref().map(|owner| owner.as_str())),
        Some(Some("v"))
    );
    assert!(!columns.owners().contains_key("y"));
    let (parts, filters, residual) = placement(
        "SELECT * FROM (VALUES (1)) v(x) CROSS JOIN ((VALUES (2)) v(x) CROSS JOIN (VALUES (3)) b(y)) AS j WHERE v.x > 1 AND j.y > 2",
    );
    assert_eq!(
        filters,
        placed(&[("v", parts[0].clone()), ("j", parts[1].clone())])
    );
    assert_eq!(residual, block.r#where.clone());

    // An unaliased VALUES list nested in an unaliased join still exposes `column1` without a qualifier.
    let (parts, filters, residual) = placement(
        "SELECT * FROM (VALUES (1)) a(x) CROSS JOIN ((VALUES (2)) b(y) CROSS JOIN (VALUES (3))) WHERE column1 > 0 AND y > 1",
    );
    assert_eq!(
        filters,
        placed(&[("b", qualify_unqualified_columns(&parts[1], "b"))])
    );
    assert_eq!(residual, Some(parts[0].clone()));
}
