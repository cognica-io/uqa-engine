//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::binding::{fixture::empty_binding_context, preparation::BLOCK_ANALYSES};
use crate::plan::{RelationalPlan, UnifiedPlan};

struct Routines;

impl FunctionTypeResolver for Routines {
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

impl RoutineResolution for Routines {}

fn block(sql: &str) -> Box<QueryBlockPlan> {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query");
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("expected query block");
    };
    block
}

#[test]
fn clause_reference_and_boolean_checks_bind_each_subquery_once() {
    let context = empty_binding_context();
    let source = RowSchema::with_qualified_types(
        "outer_row",
        vec!["id".into()],
        vec![Some(ColumnType::Integer)],
    );
    for sql in [
        "SELECT 1 WHERE (SELECT outer_row.id) > 0",
        "SELECT 1 WHERE EXISTS (SELECT outer_row.id)",
        "SELECT 1 WHERE outer_row.id IN (SELECT 1)",
        "SELECT 1 HAVING (SELECT outer_row.id) > 0",
    ] {
        let block = block(sql);
        let mut scope = SchemaScope::for_analysis(&context).unwrap();
        BLOCK_ANALYSES.with(|count| count.set(0));
        scope
            .validate_query_block_clauses(
                &Routines,
                &block,
                &source,
                &RowSchema::default(),
                &[],
                None,
            )
            .unwrap();
        let count = BLOCK_ANALYSES.with(std::cell::Cell::get);
        assert_eq!(count, 1, "{sql}: repeated subquery analysis");
    }
}

#[test]
fn condition_binding_keeps_parameters_outer_types_and_error_order_live() {
    let context = empty_binding_context();
    for (sql, column_type, params, expected) in [
        (
            "SELECT 1 WHERE (SELECT outer_row.id)",
            ColumnType::Boolean,
            vec![],
            None,
        ),
        (
            "SELECT 1 WHERE (SELECT outer_row.id)",
            ColumnType::Integer,
            vec![],
            Some("42804"),
        ),
        (
            "SELECT 1 WHERE (SELECT $1)",
            ColumnType::Integer,
            vec![SQLParam::typed_scalar(
                uqa_core::Value::Bool(true),
                ColumnType::Boolean,
            )],
            None,
        ),
        (
            "SELECT 1 WHERE (SELECT $1)",
            ColumnType::Integer,
            vec![SQLParam::typed_scalar(
                uqa_core::Value::Int(1),
                ColumnType::Integer,
            )],
            Some("42804"),
        ),
        (
            "SELECT 1 WHERE missing_name > (SELECT 1)",
            ColumnType::Integer,
            vec![],
            Some("42703"),
        ),
    ] {
        let block = block(sql);
        let source = RowSchema::with_qualified_types(
            "outer_row",
            vec!["id".into()],
            vec![Some(column_type)],
        );
        let result = SchemaScope::for_analysis(&context)
            .unwrap()
            .validate_query_block_clauses(
                &Routines,
                &block,
                &source,
                &RowSchema::default(),
                &params,
                None,
            );
        assert_eq!(
            result.as_ref().err().and_then(SQLError::sqlstate),
            expected,
            "{sql}: {result:?}"
        );
    }
}

#[test]
fn condition_subquery_results_require_the_original_arena_and_complete_outer_schema() {
    use crate::binding::type_resolution::BoundConditionQueries;
    let original = block("SELECT 1 WHERE (SELECT outer_row.id) > 0");
    let separate = original.clone();
    let input = RowSchema::with_qualified_types(
        "outer_row",
        vec!["id".into()],
        vec![Some(ColumnType::Integer)],
    );
    let output = RowSchema::with_types(vec!["id".into()], vec![Some(ColumnType::Integer)]);
    let schemas = [Some(output.clone())];
    let bound = BoundConditionQueries {
        plans: &original.subqueries,
        input: &input,
        schemas: Some(&schemas),
    };
    assert!(bound
        .get(0, &input.clone(), &original.subqueries)
        .unwrap()
        .shares_index(&output));
    assert!(bound.get(0, &input, &separate.subqueries).is_none());
    let changed_type = RowSchema::with_qualified_types(
        "outer_row",
        vec!["id".into()],
        vec![Some(ColumnType::Text)],
    );
    assert!(bound.get(0, &changed_type, &original.subqueries).is_none());
    let changed_qualifier = RowSchema::with_qualified_types(
        "inner_row",
        vec!["id".into()],
        vec![Some(ColumnType::Integer)],
    );
    assert!(bound
        .get(0, &changed_qualifier, &original.subqueries)
        .is_none());
    assert!(bound.get(1, &input, &original.subqueries).is_none());
}
