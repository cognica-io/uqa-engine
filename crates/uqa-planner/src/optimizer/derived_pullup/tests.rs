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
        assert!(matches!(block.from, Some(SourcePlan::Values { .. })));
        assert_eq!(
            block.projections[0].expr,
            ScalarExpr::qualified_column("v", "x")
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
fn repeated_input_qualifiers_are_not_captured_by_a_different_query_scope() {
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
        &BTreeSet::from(["s".into(), "v".into()]),
        &Context {
            routines: &Catalog,
            volatility: &Catalog,
            relations_bound: false,
        },
    )
    .unwrap();
    assert!(candidate.is_none());
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
