//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    ast::{FunctionBinding, FunctionVolatility},
    ColumnType,
};

struct Catalog;
impl uqa_sql::FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl uqa_sql::routines::RoutineResolution for Catalog {}
impl uqa_sql::semantics::volatility::VolatilityCatalog for Catalog {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
}

fn analyzed(body: &str, predicate: &str) -> QueryPlan {
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile(&format!("SELECT * FROM ({body}) s WHERE {predicate}"))
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
        panic!("derived")
    };
    let RelationalPlan::QueryBlock(inner) = &body.root else {
        panic!("inner block")
    };
    let columns = uqa_sql::semantics::projection_columns(&inner.projections);
    let types = vec![Some(ColumnType::Integer); columns.len()];
    body.analyzed_output = Some(RowSchema::with_types(columns.clone(), types.clone()));
    body.analyzed_source = Some(std::sync::Arc::new(AnalyzedSource {
        schema: RowSchema::with_qualified_types(
            "v",
            vec!["x".into()],
            vec![Some(ColumnType::Integer)],
        ),
        children: None,
    }));
    query.analyzed_output = Some(RowSchema::with_types(columns.clone(), types.clone()));
    query.analyzed_source = Some(std::sync::Arc::new(AnalyzedSource {
        schema: RowSchema::with_qualified_types("s", columns, types),
        children: None,
    }));
    *query
}

fn lift(query: &mut QueryPlan) {
    query_block(
        query,
        &Context {
            routines: &Catalog,
            volatility: &Catalog,
            relations_bound: false,
        },
    )
    .unwrap();
}

#[test]
fn relation_authority_boundaries_retain_their_query_and_qualification() {
    for predicate in ["k>0", "nextval('effect')>k"] {
        let mut query = analyzed(
            "SELECT x AS k,10/x AS z FROM (VALUES(0),(2))v(x)",
            predicate,
        );
        let RelationalPlan::QueryBlock(root) = &mut query.root else {
            panic!("root")
        };
        let Some(SourcePlan::Subquery { body, .. }) = &mut root.from else {
            panic!("source")
        };
        body.relation_privilege_subject = Some("input_reader".into());
        let before = serde_json::to_value(&query).unwrap();
        lift(&mut query);
        predicates::push(
            &mut query,
            &Context {
                routines: &Catalog,
                volatility: &Catalog,
                relations_bound: false,
            },
        );
        assert_eq!(serde_json::to_value(&query).unwrap(), before);
    }
}

#[test]
fn volatile_and_subquery_qualifications_precede_pure_outputs() {
    for predicate in ["nextval('effect')%2=0", "k=(SELECT 2)"] {
        let mut query = analyzed(
            "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)",
            predicate,
        );
        let descriptor = query.analyzed_output.clone().unwrap();
        lift(&mut query);
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block")
        };
        let Some(SourcePlan::Subquery {
            captured_output: Some(relation),
            ..
        }) = &block.from
        else {
            panic!("captured source")
        };
        assert_eq!(
            block.projections[0].expr,
            ScalarExpr::InternalColumn(relation.column(0))
        );
        assert_eq!(block.projections[1].alias.as_deref(), Some("z"));
        assert_eq!(
            block.subqueries.len(),
            usize::from(predicate.contains("SELECT"))
        );
        assert!(query
            .analyzed_output
            .as_ref()
            .unwrap()
            .has_same_physical_layout(&descriptor));
    }
}

#[test]
fn observable_producer_boundaries_keep_their_query_level() {
    for body in [
        "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x) OFFSET 0",
        "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x) LIMIT 1",
        "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)",
        "SELECT x AS k,nextval('effect') AS z FROM (VALUES(0),(2)) v(x)",
        "SELECT x AS k,unnest(ARRAY[1,2]) AS z FROM (VALUES(0),(2)) v(x)",
        "SELECT x AS k,row_number() OVER () AS z FROM (VALUES(0),(2)) v(x)",
    ] {
        let mut query = analyzed(body, "k=(SELECT 2)");
        lift(&mut query);
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        assert!(
            matches!(block.from, Some(SourcePlan::Subquery { .. })),
            "{body}"
        );
    }
}

#[test]
fn independent_inputs_keep_distinct_structural_identities() {
    let query = analyzed(
        "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)",
        "k=(SELECT 2)",
    );
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let candidate = candidate(
        block.from.as_ref().unwrap(),
        query.analyzed_source.as_ref().unwrap(),
        &Context {
            routines: &Catalog,
            volatility: &Catalog,
            relations_bound: false,
        },
        &mut Vec::new(),
    )
    .unwrap();
    let first = candidate.expect("capture independent source identities");
    let second = super::candidate(
        block.from.as_ref().unwrap(),
        query.analyzed_source.as_ref().unwrap(),
        &Context {
            routines: &Catalog,
            volatility: &Catalog,
            relations_bound: false,
        },
        &mut Vec::new(),
    )
    .unwrap()
    .unwrap();
    let SourcePlan::Subquery {
        captured_output: Some(first_id),
        ..
    } = first.source
    else {
        panic!("first capture")
    };
    let SourcePlan::Subquery {
        captured_output: Some(second_id),
        ..
    } = second.source
    else {
        panic!("second capture")
    };
    assert_ne!(first_id, second_id);
    let key = ColumnIdentity::qualified("s", "k");
    assert_eq!(
        first.outputs[&key],
        ScalarExpr::InternalColumn(first_id.column(0))
    );
    assert_eq!(
        second.outputs[&key],
        ScalarExpr::InternalColumn(second_id.column(0))
    );
}

#[test]
fn quoted_column_identity_and_whole_row_references_remain_distinct() {
    let schema = RowSchema::with_qualified_types(
        "s",
        vec!["k".into(), "K".into()],
        vec![Some(ColumnType::Integer); 2],
    );
    assert_eq!(
        qualified_identity(&ColumnIdentity::unqualified("K"), &schema),
        Some(ColumnIdentity::qualified("s", "K"))
    );
    assert!(qualified_identity(&ColumnIdentity::unqualified("s"), &schema).is_none());
}

#[test]
fn stored_relation_lookup_modes_keep_their_definition_boundary() {
    let mut query = analyzed(
        "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)",
        "k=(SELECT 2)",
    );
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
        panic!("derived")
    };
    body.relations_bound = true;
    lift(&mut query);
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block")
    };
    assert!(matches!(block.from, Some(SourcePlan::Subquery { .. })));
}

#[test]
fn ordinary_sources_require_no_pullup_workspace() {
    let query = analyzed(
        "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x) WHERE x>0",
        "k=2",
    );
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block")
    };
    let Some(SourcePlan::Subquery { mut body, .. }) = block.from else {
        panic!("derived")
    };
    let allocations = allocation_counter::measure(|| {
        for _ in 0..4 {
            lift(&mut body);
        }
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(allocations.bytes_total, 0);
}

#[test]
fn input_capture_reads_only_demanded_values_and_keeps_producer_filter() {
    let mut query = analyzed(
        "SELECT x AS k,10/x AS z FROM (VALUES(0,99),(2,99)) v(x,unused) WHERE x IS NOT NULL",
        "k=(SELECT 2)",
    );
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
        panic!("derived")
    };
    body.analyzed_source = Some(std::sync::Arc::new(AnalyzedSource {
        schema: RowSchema::with_qualified_types(
            "v",
            vec!["x".into(), "unused".into()],
            vec![Some(ColumnType::Integer); 2],
        ),
        children: None,
    }));
    lift(&mut query);
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let Some(SourcePlan::Subquery {
        body,
        captured_output: Some(_),
        ..
    }) = &block.from
    else {
        panic!("capture")
    };
    let RelationalPlan::QueryBlock(input) = &body.root else {
        panic!("input")
    };
    assert_eq!(input.projections.len(), 1);
    assert_eq!(
        input.projections[0].expr,
        ScalarExpr::qualified_column("v", "x")
    );
    assert!(input.r#where.is_some());
    assert_eq!(body.analyzed_output.as_ref().unwrap().columns(), ["x"]);
}

#[test]
fn captured_source_keeps_pure_predicates_at_its_scan_without_duplicate_evaluation() {
    let mut query = analyzed("SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)", "k>0");
    lift(&mut query);
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    assert!(block.r#where.is_none());
    let Some(SourcePlan::Subquery {
        body,
        captured_output: Some(_),
        ..
    }) = &block.from
    else {
        panic!("capture")
    };
    let RelationalPlan::QueryBlock(input) = &body.root else {
        panic!("input")
    };
    assert!(input.r#where.is_some());
    let mut columns = Vec::new();
    input.r#where.as_ref().unwrap().visit(&mut |node| {
        if let ScalarExpr::QualifiedColumn { qualifier, column } = node {
            columns.push((qualifier.clone(), column.to_string()));
        }
        assert!(!matches!(node, ScalarExpr::InternalColumn(_)));
    });
    assert_eq!(columns, [("v".to_owned(), "x".to_owned())]);
}

#[test]
fn stored_query_definitions_never_retain_transient_input_captures() {
    for sql in [
        "CREATE VIEW retained_capture AS SELECT 1",
        "CREATE MATERIALIZED VIEW retained_capture AS SELECT 1",
        "PREPARE retained_capture AS SELECT 1",
    ] {
        let mut plan = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        plan.visit_query_roots_mut(&mut |query| {
            *query = analyzed(
                "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)",
                "k=(SELECT 2)",
            );
        });
        let before = serde_json::to_value(&plan).unwrap();
        rewrite_plan(
            &mut plan,
            &Context {
                routines: &Catalog,
                volatility: &Catalog,
                relations_bound: false,
            },
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&plan).unwrap(), before, "{sql}");
    }
}

#[test]
fn field_qualification_and_ordering_keep_selected_input_identity() {
    for predicate in [
        "(s).k>0",
        "k=(SELECT 2) ORDER BY k",
        "k=(SELECT 2) ORDER BY 1",
        "k=(SELECT 2) ORDER BY s.k+1",
    ] {
        let mut query = analyzed(
            "SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)",
            predicate,
        );
        lift(&mut query);
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block")
        };
        assert!(
            matches!(
                block.from,
                Some(SourcePlan::Subquery {
                    captured_output: Some(_),
                    ..
                })
            ),
            "{predicate}"
        );
        if predicate.ends_with("ORDER BY k") {
            assert!(
                matches!(&block.order_by[0].expr, ScalarExpr::Column(name) if name.as_str()=="k")
            );
        }
        if predicate.ends_with("ORDER BY 1") {
            assert!(matches!(
                &block.order_by[0].expr,
                ScalarExpr::Literal(uqa_core::Value::Int(1)) | ScalarExpr::Position(0)
            ));
        }
    }
}

#[test]
fn ordering_output_alias_wins_over_input_but_compound_expression_uses_input() {
    let schema =
        RowSchema::with_qualified_types("s", vec!["k".into()], vec![Some(ColumnType::Integer)]);
    for (sql, input_reference) in [
        ("SELECT -k AS k FROM s ORDER BY k", false),
        ("SELECT -k AS k FROM s ORDER BY k+1", true),
    ] {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("block")
        };
        assert!(qualify_block(&mut block, &schema));
        let mut qualified = false;
        block.order_by[0]
            .expr
            .visit(&mut |node| qualified |= matches!(node, ScalarExpr::QualifiedColumn { .. }));
        assert_eq!(qualified, input_reference, "{sql}");
    }
}

#[test]
fn independent_scalar_predicate_moves_once_below_distinct_outputs() {
    let mut query = analyzed(
        "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2))v(x)",
        "k=(SELECT 2)",
    );
    predicates::push(
        &mut query,
        &Context {
            routines: &Catalog,
            volatility: &Catalog,
            relations_bound: false,
        },
    );
    let RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    assert!(block.r#where.is_none());
    assert!(block.subqueries.is_empty());
    let Some(SourcePlan::Subquery { body, .. }) = &block.from else {
        panic!("producer")
    };
    let RelationalPlan::QueryBlock(inner) = &body.root else {
        panic!("producer block")
    };
    assert!(inner.distinct);
    assert_eq!(inner.subqueries.len(), 1);
    let mut ids = BTreeSet::new();
    uqa_sql::semantics::collect_subquery_ids(inner.r#where.as_ref().unwrap(), &mut ids);
    assert_eq!(ids, BTreeSet::from([0]));
}

#[test]
fn correlated_shared_and_sliced_scalar_predicates_keep_their_arenas() {
    for (body, predicate, shared) in [
        (
            "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2))v(x)",
            "k=(SELECT s.k)",
            false,
        ),
        (
            "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2))v(x) OFFSET 0",
            "k=(SELECT 2)",
            false,
        ),
        (
            "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2))v(x)",
            "k=(SELECT 2)",
            true,
        ),
    ] {
        let mut query = analyzed(body, predicate);
        if shared {
            let RelationalPlan::QueryBlock(block) = &mut query.root else {
                panic!("block")
            };
            block.projections[0].expr = ScalarExpr::ScalarSubquery(0);
        }
        let before = serde_json::to_value(&query).unwrap();
        predicates::push(
            &mut query,
            &Context {
                routines: &Catalog,
                volatility: &Catalog,
                relations_bound: false,
            },
        );
        assert_eq!(
            serde_json::to_value(&query).unwrap(),
            before,
            "{body}: {predicate}"
        );
    }
}

#[test]
fn source_free_exists_preserves_three_valued_qualification() {
    for (predicate, expected) in [
        (
            "EXISTS(SELECT 1 WHERE k>0)",
            uqa_sql::ast::BooleanTest::IsTrue,
        ),
        (
            "EXISTS(SELECT 1 WHERE NULLIF(k,0)>0)",
            uqa_sql::ast::BooleanTest::IsTrue,
        ),
    ] {
        let mut query = analyzed(
            "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2))v(x)",
            predicate,
        );
        predicates::push(
            &mut query,
            &Context {
                routines: &Catalog,
                volatility: &Catalog,
                relations_bound: false,
            },
        );
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block")
        };
        assert!(block.r#where.is_none());
        let Some(SourcePlan::Subquery { body, .. }) = &block.from else {
            panic!("producer")
        };
        let RelationalPlan::QueryBlock(inner) = &body.root else {
            panic!("producer block")
        };
        let Some(ScalarExpr::Func {
            binding: Some(binding),
            ..
        }) = &inner.r#where
        else {
            panic!("Boolean qualification")
        };
        assert_eq!(
            binding.dispatch,
            Some(uqa_sql::ast::FunctionDispatch::BooleanTest(expected))
        );
    }
}

#[test]
fn exists_with_cardinality_or_effect_boundaries_is_not_inlined() {
    for predicate in [
        "NOT EXISTS(SELECT 1 WHERE k>0)",
        "EXISTS(SELECT count(*) WHERE k>0)",
        "EXISTS(SELECT unnest(ARRAY[]::int[]) WHERE k>0)",
        "EXISTS(SELECT 1 WHERE k>0 OFFSET 0)",
        "EXISTS(SELECT 1 WHERE k>nextval('effect'))",
    ] {
        let mut query = analyzed(
            "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2))v(x)",
            predicate,
        );
        let before = serde_json::to_value(&query).unwrap();
        predicates::push(
            &mut query,
            &Context {
                routines: &Catalog,
                volatility: &Catalog,
                relations_bound: false,
            },
        );
        assert_eq!(serde_json::to_value(&query).unwrap(), before, "{predicate}");
    }
}

#[test]
fn scalar_output_without_a_query_qualification_allocates_no_transfer_workspace() {
    let mut query = analyzed(
        "SELECT DISTINCT x AS k,10/x AS z FROM (VALUES(0),(2))v(x)",
        "k>0",
    );
    let UnifiedPlan::Query(extra) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT (SELECT 1)").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(extra) = extra.root else {
        panic!("block")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.subqueries = extra.subqueries;
    block.projections[0].expr = ScalarExpr::ScalarSubquery(0);
    let allocation = allocation_counter::measure(|| {
        predicates::push(
            &mut query,
            &Context {
                routines: &Catalog,
                volatility: &Catalog,
                relations_bound: false,
            },
        );
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
}
