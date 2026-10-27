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

impl RoutineResolution for Routines {
    fn has_registered_table_function(&self, name: &str) -> bool {
        matches!(name, "host_rows" | "host_declared" | "host_empty")
    }

    fn registered_table_function_columns(&self, name: &str) -> Option<Vec<String>> {
        match name {
            "host_declared" => Some(vec!["value".into(), "label".into()]),
            "host_empty" => Some(Vec::new()),
            _ => None,
        }
    }
}

#[test]
fn declared_host_columns_bind_before_execution_including_aliases_and_groups() {
    for (sql, expected) in [
        ("SELECT * FROM host_declared(3)", vec!["value", "label"]),
        (
            "SELECT * FROM host_declared(3) AS h(renamed)",
            vec!["renamed", "label"],
        ),
        (
            "SELECT * FROM host_declared(3) WITH ORDINALITY",
            vec!["value", "label", "ordinality"],
        ),
        (
            "SELECT * FROM ROWS FROM (host_declared(3), host_declared(4)) AS h(a,b,c,d)",
            vec!["a", "b", "c", "d"],
        ),
    ] {
        let block = block(sql);
        let schema = SchemaScope::for_analysis(&empty_binding_context())
            .unwrap()
            .bind_source(
                &Routines,
                block.from.as_ref().unwrap(),
                &block.subqueries,
                &[],
                None,
            )
            .unwrap();
        assert!(!schema.columns_are_open(None), "{sql}");
        let names = (0..schema.len())
            .map(|index| schema.public_name(index).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, expected, "{sql}");
    }
    let block = block("SELECT * FROM host_rows(3)");
    let schema = SchemaScope::for_analysis(&empty_binding_context())
        .unwrap()
        .bind_source(
            &Routines,
            block.from.as_ref().unwrap(),
            &block.subqueries,
            &[],
            None,
        )
        .unwrap();
    assert!(schema.columns_are_open(None));
}

#[test]
fn runtime_group_width_defers_alias_validation_and_positional_types() {
    let bind = |sql| {
        let statement = block(sql);
        SchemaScope::for_analysis(&empty_binding_context())
            .unwrap()
            .bind_source(
                &Routines,
                statement.from.as_ref().unwrap(),
                &statement.subqueries,
                &[],
                None,
            )
    };
    let schema = bind(
        "SELECT * FROM ROWS FROM(host_rows(2), json_each('{}'::json)) WITH ORDINALITY h(a,b,c,d,e)",
    )
    .unwrap();
    assert!(schema.columns_are_open(None));
    assert!(schema.column_types().iter().all(Option::is_none));
    let schema = bind(
        "SELECT * FROM ROWS FROM(json_each('{}'::json), host_rows(2)) WITH ORDINALITY h(a,b,c,d,e)",
    )
    .unwrap();
    assert!(schema.columns_are_open(None));
    assert_eq!(schema.column_type(0), Some(&ColumnType::Text));
    assert_eq!(schema.column_type(1), Some(&ColumnType::Json));
    assert!(schema.column_types()[2..].iter().all(Option::is_none));
    let error = bind("SELECT * FROM ROWS FROM(host_declared(2)) h(a,b,c)").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P10"));
    let error = bind("SELECT * FROM host_empty(2)").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
}

#[test]
fn registered_table_calls_are_validated_before_set_placement() {
    for (name, expected) in [("host_rows", true), ("missing_host_rows", false)] {
        let block = block(&format!("SELECT {name}(3)"));
        let expression = &block.projections[0].expr;
        let ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            ..
        } = expression
        else {
            panic!("function expression");
        };
        let schema = RowSchema::default();
        let result = functions::validate_scalar_function(
            &Routines,
            functions::ScalarFunctionValidation {
                name,
                binding: binding.as_ref(),
                args,
                order_by,
                expression,
                schema: &schema,
                params: &[],
                resolver: &Routines,
            },
        );
        assert_eq!(result.is_ok(), expected, "{result:?}");
        assert_eq!(
            crate::semantics::sets::validation::function_may_return_set(
                &Routines,
                &Routines,
                name,
                binding.as_ref(),
                args,
                &schema,
                &[],
            )
            .unwrap(),
            expected
        );
    }
}

#[test]
fn expression_analysis_resolves_parameters_in_its_supplied_scope() {
    let context = empty_binding_context();
    let parameters = [SQLParam::typed_scalar(
        uqa_core::Value::Int(7),
        ColumnType::Integer,
    )];
    for (number, supplied, expected) in [
        (0, &parameters[..], Some("there is no parameter $0")),
        (1, &[][..], Some("there is no parameter $1")),
        (2, &parameters[..], Some("there is no parameter $2")),
        (1, &parameters[..], None),
    ] {
        let expression = crate::plan::ExpressionPlan::lower(crate::ast::Expr::Param(number));
        let result = crate::binding::analyze_expression_plan_type(
            &Routines,
            &expression,
            supplied,
            &context,
        );
        if let Some(expected) = expected {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("42P02"));
            assert_eq!(error.to_string(), expected);
        } else {
            assert_eq!(result.unwrap(), Some(ColumnType::Integer));
        }
    }
}

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
