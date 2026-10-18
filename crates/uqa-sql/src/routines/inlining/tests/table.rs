//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn table_inlining_retains_result_assignment_and_external_parameters() {
    let catalog = Catalog::new("CREATE FUNCTION widened(x integer) RETURNS SETOF bigint LANGUAGE sql STABLE AS $$SELECT x$$");
    let binding = catalog.binding("widened", &[Some(ColumnType::Integer)]);
    let replacement = catalog
        .context()
        .prepare_table(&binding, &[ScalarExpr::Param(3)], &[])
        .unwrap()
        .unwrap();
    assert!(replacement.declared_columns.is_none());
    let RelationalPlan::QueryBlock(block) = &replacement.query.root else {
        panic!("SELECT body")
    };
    let ScalarExpr::Cast { expr, ty, .. } = &block.projections[0].expr else {
        panic!("return assignment")
    };
    assert_eq!(
        ColumnType::from_sql_name(ty).unwrap(),
        ColumnType::BigInteger
    );
    assert!(matches!(expr.as_ref(), ScalarExpr::Param(3)));
    assert!(catalog
        .context()
        .prepare_table(&binding, &[integer(1)], &[replacement.identity])
        .unwrap()
        .is_none());
}

#[test]
fn table_inlining_respects_snapshot_strictness_configuration_and_security_boundaries() {
    for attributes in [
        "VOLATILE",
        "STABLE STRICT",
        "STABLE SECURITY DEFINER",
        "STABLE SET search_path=public",
    ] {
        let catalog = Catalog::new(&format!("CREATE FUNCTION bounded(x integer) RETURNS SETOF integer LANGUAGE sql {attributes} AS $$SELECT x$$"));
        let binding = catalog.binding("bounded", &[Some(ColumnType::Integer)]);
        assert!(
            catalog
                .context()
                .prepare_table(&binding, &[integer(1)], &[])
                .unwrap()
                .is_none(),
            "{attributes}"
        );
    }
    let mut catalog = Catalog::new("CREATE FUNCTION bounded(x integer) RETURNS SETOF integer LANGUAGE sql STABLE AS $$SELECT x$$");
    Arc::get_mut(&mut catalog.functions[0])
        .unwrap()
        .def
        .execute_acl = Some(Vec::new());
    let binding = catalog.binding("bounded", &[Some(ColumnType::Integer)]);
    catalog.allowed.store(false, Ordering::SeqCst);
    assert!(catalog
        .context()
        .prepare_table(&binding, &[integer(1)], &[])
        .unwrap()
        .is_none());
}

#[test]
fn ordinary_table_calls_replan_without_caching_function_body_results() {
    let catalog = Catalog::new(
        "CREATE FUNCTION fresh() RETURNS SETOF integer LANGUAGE sql STABLE AS $$SELECT 1$$",
    );
    let binding = catalog.binding("fresh", &[]);
    for sql in [
        "SELECT * FROM fresh()",
        "SELECT EXISTS(SELECT * FROM fresh())",
        "WITH r AS (SELECT * FROM fresh()) SELECT * FROM r",
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        query.visit_sources_mut(&mut |source| {
            if let crate::plan::SourcePlan::Function {
                binding: selected, ..
            } = source
            {
                *selected = Some(binding.clone());
            }
        });
        assert!(catalog.context().requires_replanning(&plan), "{sql}");
    }
}

#[test]
fn table_inlining_preserves_declared_names_for_out_parameters() {
    let catalog = Catalog::new("CREATE FUNCTION named(x integer) RETURNS TABLE(result bigint) LANGUAGE sql STABLE AS $$SELECT x$$");
    let binding = catalog.binding("named", &[Some(ColumnType::Integer)]);
    let replacement = catalog
        .context()
        .prepare_table(&binding, &[integer(4)], &[])
        .unwrap()
        .unwrap();
    assert_eq!(replacement.declared_columns.unwrap(), ["result"]);
}
