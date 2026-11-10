//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use uqa_core::{memory::RetainedValue, RelationIdentity};
use uqa_sql::{
    ast::{Expr, FunctionBinding, FunctionCallSyntax, Statement, TableConstraintSet},
    plan::UnifiedPlan,
    routines::SQLUserFunction,
    ColumnType,
};

fn call(identity: u8) -> Expr {
    Expr::Func {
        name: "public.original".into(),
        binding: Some(FunctionBinding {
            object_id: Some([identity; 16]),
            name: "public.original".into(),
            argument_types: vec!["int4".into()],
            builtin: false,
            dispatch: None,
            invocation: None,
            value_binding: None,
            resolution_error: None,
            input_collation: None,
        }),
        args: vec![Expr::Literal(uqa_core::Value::Int(1))],
        distinct: false,
        order_by: vec![],
        order_syntax: FunctionCallSyntax::Ordinary,
        filter: None,
    }
}

fn function() -> uqa_sql::ast::CreateFunction {
    let Statement::CreateFunction(mut function) = uqa_sql::compile(
        "CREATE FUNCTION public.renamed(n integer) RETURNS integer LANGUAGE SQL AS 'SELECT missing(n)'",
    )
    .unwrap()
    .remove(0) else {
        panic!("function");
    };
    function.object_id = Some([1; 16]);
    *function
}

fn snapshot() -> CatalogReadSnapshot {
    let mut snapshot = empty_catalog().snapshot().clone();
    RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
        .insert(
            "public.renamed".into(),
            vec![SQLUserFunction::new(function(), RoutineBody::Source).shared()],
        )
        .unwrap();
    snapshot
}

fn check(snapshot: &CatalogReadSnapshot) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(1 << 20);
    let result = validate(snapshot, &memory, &CancellationToken::new());
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_routine_closure_checks_stored_column_bindings_without_resolving_lazy_bodies() {
    for identity in [1, 2] {
        let mut snapshot = snapshot();
        let mut column = ColumnDef::nullable("value", ColumnType::Integer);
        column.default = Some(call(identity));
        snapshot.tables.insert(
            RelationIdentity::new("public", "items"),
            table_snapshot([3; 16], vec![column], TableConstraintSet::default()),
        );
        assert_eq!(check(&snapshot).is_ok(), identity == 1);
    }
}

#[test]
fn native_routine_closure_checks_parameter_defaults_and_retained_sql_actions() {
    for identity in [1, 2] {
        for bound in [false, true] {
            let mut snapshot = snapshot();
            let mut definition = function();
            definition.name = "public.caller".into();
            definition.object_id = Some([3; 16]);
            let body = if bound {
                let statement = Statement::Values {
                    rows: vec![vec![call(identity)]],
                };
                definition.body = FunctionBody::Statements(vec![statement.clone()]);
                RoutineBody::Bound(
                    CompiledFunctionBody::SQL(vec![UnifiedPlan::lower(statement)]).shared(),
                )
            } else {
                definition.params[0].default = Some(call(identity));
                RoutineBody::Source
            };
            RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
                .insert(
                    "public.caller".into(),
                    vec![SQLUserFunction::new(definition, body).shared()],
                )
                .unwrap();
            assert_eq!(check(&snapshot).is_ok(), identity == 1);
        }
    }
}

#[test]
fn native_routine_closure_releases_workspace_on_admission_denial_and_cancellation() {
    let snapshot = snapshot();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(0);
    assert!(validate(&snapshot, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    let memory = MemoryBudget::new(1 << 20);
    token.cancel();
    assert!(validate(&snapshot, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_trigger_references_require_the_selected_trigger_function_contract() {
    for (returns, language, valid) in [
        ("trigger", "plpgsql", true),
        ("SETOF trigger", "plpgsql", true),
        ("integer", "plpgsql", false),
        ("trigger", "sql", false),
    ] {
        let mut snapshot = empty_catalog().snapshot().clone();
        let Statement::CreateFunction(mut function) = uqa_sql::compile(&format!(
            "CREATE FUNCTION public.fire() RETURNS {returns} LANGUAGE {language} AS 'uncompiled source body'",
        )).unwrap().remove(0) else { panic!("function"); };
        function.object_id = Some([1; 16]);
        RetainedValue::make_mut(&mut snapshot.definitions.sql_user_functions)
            .insert(
                "public.fire".into(),
                vec![SQLUserFunction::new(*function, RoutineBody::Source).shared()],
            )
            .unwrap();
        let Statement::CreateTrigger(definition) = uqa_sql::compile(
            "CREATE TRIGGER saved BEFORE INSERT ON public.items FOR EACH ROW EXECUTE FUNCTION public.fire()",
        ).unwrap().remove(0) else { panic!("trigger"); };
        RetainedValue::make_mut(&mut snapshot.definitions.triggers)
            .insert_event(
                RelationIdentity::new("public", "items"),
                "saved".into(),
                uqa_sql::catalog::events::StoredTrigger {
                    definition,
                    function_object_id: Some([1; 16]),
                    enabled: uqa_sql::ast::EventEnableMode::default(),
                    object_id: None,
                    constraint_name: None,
                    catalog_oid: None,
                    constraint_catalog_oid: None,
                },
            )
            .unwrap();
        let result = check(&snapshot);
        assert_eq!(result.is_ok(), valid, "{returns}/{language}: {result:?}");
    }
}
