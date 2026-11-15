//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{Expr, FunctionBinding, FunctionDispatch, RoutineInvocationBinding, RoutineVariadicMode},
    plan::UnifiedPlan,
};

#[test]
fn native_expression_types_validate_nested_syntax_and_table_function_outputs() {
    let types = Types::with_enum();
    let memory = MemoryBudget::new(1 << 20);
    let cancel = CancellationToken::new();
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    let retained = memory.used();
    for template in [
        "SELECT NULL::\"enum#OID\"",
        "WITH items AS (SELECT NULL::\"enum#OID\" AS v) SELECT v FROM items",
        "SELECT (SELECT NULL::\"enum#OID\")",
        "INSERT INTO public.items VALUES (NULL::\"enum#OID\")",
        "SELECT v FROM json_to_record('{}') AS t(v \"enum#OID\")",
        "SELECT v FROM ROWS FROM (json_to_record('{}') AS (v \"enum#OID\")) AS t",
    ] {
        for oid in [20_000, 20_002] {
            let sql = template.replace("OID", &oid.to_string());
            let mut statement = crate::compile(&sql).unwrap().remove(0);
            crate::binding::stored_types::upgrade_stored_statement_type_names(
                &mut statement,
                &mut |_, _| Ok(Some(format!("enum#{oid}"))),
            )
            .unwrap();
            let before = serde_json::to_value(&statement).unwrap();
            let result = index.statement(&statement);
            assert_eq!(result.is_ok(), oid == 20_000, "{sql}: {result:?}");
            assert_eq!(serde_json::to_value(&statement).unwrap(), before);
            let plan = UnifiedPlan::lower(statement);
            assert_eq!(index.plan((&plan).into()).is_ok(), oid == 20_000, "{sql}");
            assert_eq!(memory.used(), retained);
        }
    }
    for oid in [20_000, 20_002] {
        let mut ty = mood();
        let ColumnType::Enum(reference) = &mut ty else {
            unreachable!()
        };
        reference.oid = oid;
        let scalar = crate::ScalarExpr::TypedLiteral {
            value: uqa_core::Value::Null,
            ty: "enum#20000".into(),
            collation_oid: None,
            composite_source: None,
            bound_type: Some(ty.clone()),
            parameter_index: None,
        };
        assert_eq!(index.scalar(&scalar).is_ok(), oid == 20_000);
        let mut plan = UnifiedPlan::lower(crate::compile("SELECT *").unwrap().remove(0));
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        let crate::plan::RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("query block")
        };
        block.from = Some(crate::plan::SourcePlan::Values {
            lateral: false,
            rows: vec![vec![crate::ScalarExpr::Literal(uqa_core::Value::Null)]],
            alias: None,
            column_aliases: vec!["selected".into()],
            internal_relation: None,
            internal_column_types: vec![Some(ty)],
            internal_column_collations: Vec::new(),
        });
        assert_eq!(index.plan((&plan).into()).is_ok(), oid == 20_000);
    }
    let default = Expr::Cast {
        implicit: false,
        expr: Box::new(Expr::Literal(uqa_core::Value::Null)),
        ty: "enum#20000".into(),
    };
    index.expression(&default).unwrap();
    cancel.cancel();
    assert!(index.expression(&default).is_err());
    drop(index);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_expression_types_validate_independent_invocation_and_dispatch_metadata() {
    let types = Types::with_enum();
    let memory = MemoryBudget::new(1 << 20);
    let cancel = CancellationToken::new();
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel).unwrap();
    for location in 0..5 {
        for oid in [20_000, 20_002] {
            let name = format!("enum#{oid}");
            let mut binding = FunctionBinding::builtin_by_name("selected", None);
            let mut invocation = RoutineInvocationBinding {
                argument_positions: vec![0],
                argument_targets: vec!["text".into()],
                argument_sources: vec![Some("text".into())],
                parameter_types: vec!["text".into()],
                return_type: Some("text".into()),
                variadic_mode: RoutineVariadicMode::None,
            };
            match location {
                0 => binding.argument_types.push(name),
                1 => invocation.argument_targets[0] = name,
                2 => invocation.argument_sources[0] = Some(name),
                3 => invocation.parameter_types[0] = name,
                4 => invocation.return_type = Some(name),
                _ => unreachable!(),
            }
            binding.invocation = Some(Box::new(invocation));
            assert_eq!(index.function_types(&binding).is_ok(), oid == 20_000);
        }
    }
    for oid in [23, 1007, 2249, 20_000, 20_001, 20_002] {
        let mut binding = FunctionBinding::builtin_by_name("pg_typeof", None);
        binding.dispatch = Some(FunctionDispatch::TypeOf { type_oid: oid });
        assert_eq!(index.function_types(&binding).is_ok(), oid != 20_002);
    }
    drop(index);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_expression_type_lowering_retires_workspace_after_admission_failure() {
    let types = Types::with_enum();
    let statement =
        crate::compile("WITH selected AS (SELECT NULL::\"enum#20000\") SELECT * FROM selected")
            .unwrap()
            .remove(0);
    let cancel = CancellationToken::new();
    let mut denied = 0;
    let mut accepted = 0;
    for limit in (0..16384).step_by(256) {
        let memory = MemoryBudget::new(limit);
        if let Ok(index) = TypeReferences::new(types.input(), std::iter::empty(), &memory, &cancel)
        {
            let retained = memory.used();
            if index.statement(&statement).is_ok() {
                accepted += 1;
            } else {
                denied += 1;
            }
            assert_eq!(memory.used(), retained);
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(denied > 0 && accepted > 0);
}
