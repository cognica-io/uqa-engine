//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn correlation_uses_selected_routine_output_without_evaluating_its_body() {
    use crate::binding::correlation::{query_depends_on_outer_row, CorrelationContext};
    use crate::plan::SourcePlan;

    let catalog = Catalog::new(
        "CREATE FUNCTION picked(value bigint) RETURNS TABLE(other integer) LANGUAGE plpgsql AS $$BEGIN RETURN QUERY SELECT 8; END$$;
         CREATE FUNCTION picked(value integer) RETURNS TABLE(x integer, y integer) LANGUAGE plpgsql AS $$BEGIN RETURN QUERY SELECT 4, 5; END$$",
    );
    let binding_context = crate::binding::fixture::empty_binding_context();
    let context = CorrelationContext {
        routines: &catalog,
        catalog: binding_context.catalog.as_ref(),
        resolution: &binding_context.resolution,
        scoped_relations: None,
    };
    for (expression, dependent) in [("y", false), ("caller_value", true)] {
        let sql = format!("SELECT {expression} FROM picked(1) f(renamed_x)");
        let UnifiedPlan::Query(mut query) =
            UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        let Some(SourcePlan::Function { binding, .. }) = &mut block.from else {
            panic!("function")
        };
        *binding = Some(catalog.binding("picked", &[Some(ColumnType::Integer)]));
        assert_eq!(
            query_depends_on_outer_row(context, &query).unwrap(),
            dependent,
            "{sql}",
        );
    }
    assert_eq!(catalog.evaluations.load(Ordering::SeqCst), 0);
}

#[test]
fn result_assignment_follows_body_distinct_grouping_and_ordering() {
    for body in [
        "SELECT DISTINCT v FROM (VALUES (1.2),(1.4)) d(v)",
        "SELECT DISTINCT ON(v) v FROM (VALUES (1.2),(1.4)) d(v) ORDER BY v",
        "SELECT v FROM (VALUES (1.2),(1.4)) d(v) GROUP BY v",
        "SELECT v FROM (VALUES (1.2),(1.4)) d(v) GROUP BY GROUPING SETS((v),())",
        "SELECT v FROM (VALUES (1.2),(1.4)) d(v) ORDER BY v",
    ] {
        let catalog = Catalog::new(&format!(
            "CREATE FUNCTION assigned() RETURNS SETOF integer LANGUAGE sql STABLE AS $${body}$$"
        ));
        let binding = catalog.binding("assigned", &[]);
        let result = catalog
            .context()
            .prepare_table(&binding, &[], &[], &[], &[])
            .unwrap()
            .unwrap();
        let RelationalPlan::QueryBlock(wrapper) = &result.query.root else {
            panic!("assignment projection")
        };
        assert!(
            matches!(wrapper.projections[0].expr, ScalarExpr::Cast { .. }),
            "{body}"
        );
        let Some(crate::plan::SourcePlan::Subquery { body: original, .. }) = &wrapper.from else {
            panic!("assignment must follow the query body: {body}")
        };
        let RelationalPlan::QueryBlock(original) = &original.root else {
            panic!("original SELECT")
        };
        assert!(
            !matches!(original.projections[0].expr, ScalarExpr::Cast { .. }),
            "{body}"
        );
        assert_eq!(catalog.evaluations.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn from_overloads_use_argument_subquery_types_before_attempting_expansion() {
    let catalog = Catalog::new("CREATE FUNCTION picked(value integer) RETURNS TABLE(int4_value text) LANGUAGE SQL AS 'SELECT ''int4'''; CREATE FUNCTION picked(value bigint) RETURNS TABLE(int8_value text) LANGUAGE SQL AS 'SELECT ''int8'''");
    let context = crate::binding::fixture::empty_binding_context();
    for (sql, column) in [
        ("SELECT * FROM picked((SELECT 7::bigint))", "int8_value"),
        ("SELECT * FROM picked((SELECT 7::integer))", "int4_value"),
        (
            "SELECT * FROM ROWS FROM (picked((SELECT 7::bigint)))",
            "int8_value",
        ),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        crate::binding::infer_prepared_parameter_types(&catalog, &plan, &[], &context)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let UnifiedPlan::Query(query) = plan else {
            unreachable!()
        };
        let schema =
            crate::binding::bind_query_plan_schema(&catalog, &query, &[], &context, None).unwrap();
        assert_eq!(schema.columns(), &[column.to_string()], "{sql}");
        assert_eq!(catalog.evaluations.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn correlated_arguments_capture_only_attributes_and_keep_expression_demand_in_the_body() {
    let catalog = Catalog::new("CREATE FUNCTION conditional(x integer, flag boolean) RETURNS SETOF integer LANGUAGE sql STABLE AS $$SELECT CASE WHEN flag THEN x ELSE 7 END$$");
    let binding = catalog.binding(
        "conditional",
        &[Some(ColumnType::Integer), Some(ColumnType::Boolean)],
    );
    let argument = ScalarExpr::Binary {
        op: crate::ast::BinaryOp::Divide,
        lhs: Box::new(integer(10)),
        rhs: Box::new(ScalarExpr::qualified_column("caller", "value")),
    };
    let replacement = catalog
        .context()
        .prepare_table(
            &binding,
            &[argument, ScalarExpr::Literal(uqa_core::Value::Bool(false))],
            &[],
            &[],
            &[],
        )
        .unwrap()
        .unwrap();
    let RelationalPlan::QueryBlock(wrapper) = &replacement.query.root else {
        panic!("capture wrapper")
    };
    let Some(crate::plan::SourcePlan::Join {
        left,
        right,
        lateral: true,
        ..
    }) = &wrapper.from
    else {
        panic!("lexical capture")
    };
    let crate::plan::SourcePlan::Values {
        rows,
        internal_relation: Some(relation),
        ..
    } = left.as_ref()
    else {
        panic!("captured attributes")
    };
    assert_eq!(
        rows,
        &[vec![ScalarExpr::qualified_column("caller", "value")]]
    );
    let crate::plan::SourcePlan::Subquery { body, .. } = right.as_ref() else {
        panic!("routine body")
    };
    let RelationalPlan::QueryBlock(block) = &body.root else {
        panic!("SELECT body")
    };
    assert!(matches!(block.projections[0].expr, ScalarExpr::Case { .. }));
    let mut captured = false;
    block.projections[0].expr.visit(&mut |node| {
        captured |=
            matches!(node, ScalarExpr::InternalColumn(column) if *column == relation.column(0));
        assert!(
            !matches!(node, ScalarExpr::QualifiedColumn { qualifier, .. } if qualifier == "caller")
        );
    });
    assert!(captured);
}

#[test]
fn table_inlining_accepts_general_single_select_bodies() {
    for body in [
        "SELECT DISTINCT v FROM (VALUES (1),(2)) data(v)",
        "SELECT sum(v)::int FROM (VALUES (1),(2)) data(v)",
        "WITH data(v) AS (VALUES (1),(2)) SELECT v FROM data UNION ALL SELECT 3",
    ] {
        let catalog = Catalog::new(&format!(
            "CREATE FUNCTION rows() RETURNS SETOF integer LANGUAGE sql STABLE AS $${body}$$"
        ));
        let binding = catalog.binding("rows", &[]);
        assert!(
            catalog
                .context()
                .prepare_table(&binding, &[], &[], &[], &[])
                .unwrap()
                .is_some(),
            "{body}"
        );
    }
}

#[test]
fn table_inlining_retains_result_assignment_and_external_parameters() {
    let catalog = Catalog::new(
        "CREATE FUNCTION widened(x integer) RETURNS SETOF bigint LANGUAGE sql STABLE AS $$SELECT x$$",
    );
    let binding = catalog.binding("widened", &[Some(ColumnType::Integer)]);
    let replacement = catalog
        .context()
        .prepare_table(&binding, &[ScalarExpr::Param(3)], &[], &[], &[])
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
        .prepare_table(&binding, &[integer(1)], &[replacement.identity], &[], &[])
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
        let catalog = Catalog::new(&format!(
            "CREATE FUNCTION bounded(x integer) RETURNS SETOF integer LANGUAGE sql {attributes} AS $$SELECT x$$"
        ));
        let binding = catalog.binding("bounded", &[Some(ColumnType::Integer)]);
        assert!(
            catalog
                .context()
                .prepare_table(&binding, &[integer(1)], &[], &[], &[])
                .unwrap()
                .is_none(),
            "{attributes}"
        );
    }
    let mut catalog = Catalog::new(
        "CREATE FUNCTION bounded(x integer) RETURNS SETOF integer LANGUAGE sql STABLE AS $$SELECT x$$",
    );
    Arc::get_mut(&mut catalog.functions[0])
        .unwrap()
        .def
        .execute_acl = Some(Vec::new());
    let binding = catalog.binding("bounded", &[Some(ColumnType::Integer)]);
    catalog.allowed.store(false, Ordering::SeqCst);
    assert!(catalog
        .context()
        .prepare_table(&binding, &[integer(1)], &[], &[], &[])
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
    let catalog = Catalog::new(
        "CREATE FUNCTION named(x integer) RETURNS TABLE(result bigint) LANGUAGE sql STABLE AS $$SELECT x$$",
    );
    let binding = catalog.binding("named", &[Some(ColumnType::Integer)]);
    let replacement = catalog
        .context()
        .prepare_table(&binding, &[integer(4)], &[], &[], &[])
        .unwrap()
        .unwrap();
    assert_eq!(replacement.declared_columns.unwrap(), ["result"]);
}
