//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::bind_query_block_for_execution;
use crate::binding::fixture::empty_binding_context;
use crate::plan::{QueryBlockPlan, RelationalPlan, UnifiedPlan};
use crate::{ColumnType, FunctionTypeResolver, SQLError};
use std::borrow::Cow;

mod source_transfer;

struct Routines;

impl FunctionTypeResolver for Routines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl crate::routines::RoutineResolution for Routines {
    fn resolve_table_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<ColumnType>],
        explicit_variadic: bool,
        builtins: &[crate::type_resolution::BuiltinFunctionOverload],
    ) -> Result<Option<crate::type_resolution::ResolvedFunctionOverload>, SQLError> {
        assert!(!explicit_variadic);
        crate::type_resolution::resolve_local_builtin_overload(
            name,
            binding,
            argument_names,
            argument_types,
            builtins,
        )
        .map(Some)
    }
}

fn block(sql: &str) -> Box<QueryBlockPlan> {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = plan.root else {
        panic!("query block");
    };
    block
}

#[test]
fn execution_binding_does_not_copy_unchanged_query_payloads() {
    let mut query = block("SELECT 1 FROM (VALUES (1)) AS t(id)");
    query.projections[0].expr =
        crate::ScalarExpr::Literal(uqa_core::Value::Str("x".repeat(256 * 1024)));
    let context = empty_binding_context();
    let allocation = allocation_counter::measure(|| {
        for _ in 0..16 {
            let (execution, schema) =
                bind_query_block_for_execution(&Routines, &query, &[], &context, None, false)
                    .unwrap();
            assert_eq!(schema.column_type(0), Some(&ColumnType::Integer));
            assert!(matches!(
                &execution.projections[0].expr,
                crate::ScalarExpr::Literal(uqa_core::Value::Str(text)) if text.len() == 256 * 1024
            ));
        }
    });
    assert!(
        allocation.bytes_total < 128 * 1024,
        "unchanged execution syntax was copied: {allocation:?}"
    );
}

#[test]
fn execution_binding_isolates_distinct_slices_and_function_identities() {
    let context = empty_binding_context();
    let query =
        block("SELECT DISTINCT id FROM generate_series(1, 3) AS series(id) LIMIT 1 OFFSET 1");
    let original = serde_json::to_vec(&query).unwrap();
    for deferred in [false, true, false] {
        let (execution, schema) =
            bind_query_block_for_execution(&Routines, &query, &[], &context, None, deferred)
                .unwrap();
        assert!(matches!(execution, Cow::Owned(_)));
        assert_eq!(execution.limit.is_none(), deferred);
        assert_eq!(execution.offset.is_none(), deferred);
        assert_eq!(schema.column_type(0), Some(&ColumnType::Integer));
        let Some(crate::plan::SourcePlan::Function { binding, .. }) = &execution.from else {
            panic!("function source");
        };
        assert!(binding.as_ref().is_some_and(|binding| binding.builtin));
        assert_eq!(serde_json::to_vec(&query).unwrap(), original);
    }
}

#[test]
fn borrowed_execution_binding_keeps_live_parameters_and_outer_types() {
    let context = empty_binding_context();
    let query = block("SELECT id FROM (VALUES (outer_row.value)) AS t(id)");
    for ty in [
        ColumnType::Integer,
        ColumnType::Text,
        ColumnType::BigInteger,
    ] {
        let outer = crate::RowSchema::with_qualified_types(
            "outer_row",
            vec!["value".into()],
            vec![Some(ty.clone())],
        );
        let (execution, schema) =
            bind_query_block_for_execution(&Routines, &query, &[], &context, Some(&outer), false)
                .unwrap();
        assert!(matches!(execution, Cow::Borrowed(_)));
        assert_eq!(schema.column_type(0), Some(&ty));
    }
    let query = block("SELECT id FROM (VALUES ($1)) AS t(id)");
    for (parameter, ty) in [
        (
            crate::SQLParam::typed_scalar(uqa_core::Value::Int(7), ColumnType::Integer),
            ColumnType::Integer,
        ),
        (
            crate::SQLParam::typed_scalar(uqa_core::Value::Str("seven".into()), ColumnType::Text),
            ColumnType::Text,
        ),
    ] {
        let (execution, schema) =
            bind_query_block_for_execution(&Routines, &query, &[parameter], &context, None, false)
                .unwrap();
        assert!(matches!(execution, Cow::Borrowed(_)));
        assert_eq!(schema.column_type(0), Some(&ty));
    }
}

#[test]
fn borrowed_execution_binding_preserves_missing_relation_order() {
    let context = empty_binding_context();
    let query = block("SELECT 1 FROM missing_first, missing_second");
    let error =
        bind_query_block_for_execution(&Routines, &query, &[], &context, None, false).unwrap_err();
    assert!(matches!(error, SQLError::UnknownTable(name) if name == "missing_first"));
}
