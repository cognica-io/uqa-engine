//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{defaults::assert_work, *};
use uqa_core::Value;
use uqa_sql::{
    ast::{Expr, FunctionBinding, FunctionBody, Statement},
    plan::{RelationalPlan, SourcePlan, UnifiedPlan},
    routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
};

fn with_plans(syntax: Statement, bound: UnifiedPlan) -> CatalogReadView {
    let mut catalog = snapshot();
    let Statement::CreateFunction(mut function) = uqa_sql::compile(
        "CREATE FUNCTION public.checked() RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
    )
    .unwrap()
    .remove(0) else {
        panic!("function declaration");
    };
    function.object_id = Some([60; 16]);
    function.owner = Some(RoleIdentity::BOOTSTRAP);
    function.body = FunctionBody::Statements(vec![syntax]);
    let body = RoutineBody::Bound(CompiledFunctionBody::SQL(vec![bound]).shared());
    RetainedValue::make_mut(&mut catalog.definitions.sql_user_functions)
        .insert(
            "public.checked".into(),
            vec![SQLUserFunction::new(*function, body).shared()],
        )
        .unwrap();
    CatalogReadView::new(catalog)
}

fn values(width: usize, invalid: bool) -> Statement {
    let mut row = vec![Expr::Literal(Value::Int(1)); width];
    row.push(if invalid {
        Expr::Cast {
            implicit: false,
            expr: Box::new(Expr::Literal(Value::Null)),
            ty: "enum#999999".into(),
        }
    } else {
        Expr::Literal(Value::Null)
    });
    Statement::Values { rows: vec![row] }
}

#[test]
fn stored_routine_plan_type_checks_yield_for_original_and_bound_scalar_positions() {
    let width = COUNT as usize;
    for (syntax, bound) in [
        (values(width, true), values(1, false)),
        (values(1, false), values(width, true)),
    ] {
        assert_work(
            &with_plans(syntax, UnifiedPlan::lower(bound)),
            width,
            "missing type or different type kind",
        );
    }
}

#[test]
fn stored_routine_plan_type_checks_yield_for_bindings_and_optional_output_slots() {
    let width = COUNT as usize;
    for kind in 0..3 {
        let template = uqa_sql::compile("SELECT * FROM json_to_record('{}') AS t(v integer)")
            .unwrap()
            .remove(0);
        let mut plan = UnifiedPlan::lower(template.clone());
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("query block")
        };
        let source = block.from.as_mut().unwrap();
        let expected = if kind == 2 {
            let mut outputs = vec![None; width];
            outputs.push(Some(ColumnType::Named("lost".into())));
            *source = SourcePlan::Values {
                lateral: false,
                rows: Vec::new(),
                alias: None,
                column_aliases: Vec::new(),
                internal_relation: None,
                internal_column_types: outputs,
                internal_column_collations: Vec::new(),
            };
            "unresolved column type"
        } else {
            let SourcePlan::Function {
                binding,
                column_types,
                ..
            } = source
            else {
                panic!("function")
            };
            if kind == 0 {
                let mut original = FunctionBinding::builtin_by_name("json_to_record", None);
                original.argument_types = vec!["int4".into(); width];
                original.argument_types.push("enum#999999".into());
                *binding = Some(original);
                column_types.push("enum#bad".into());
                "missing type or different type kind"
            } else {
                *column_types = vec!["int4".into(); width];
                column_types.push("enum#bad".into());
                "malformed stored type identity"
            }
        };
        assert_work(&with_plans(template, plan), width, expected);
    }
}
