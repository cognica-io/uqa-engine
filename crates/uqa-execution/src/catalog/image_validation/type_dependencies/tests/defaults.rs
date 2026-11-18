//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;
use uqa_sql::{
    ast::{
        Expr, FunctionBinding, FunctionCallSyntax, RoutineInvocationBinding, RoutineVariadicMode,
        Statement,
    },
    routines::{RoutineBody, SQLUserFunction},
};

fn catalog_with_default(default: Expr) -> CatalogReadView {
    let mut catalog = snapshot();
    let Statement::CreateFunction(mut function) = uqa_sql::compile(
        "CREATE FUNCTION public.checked(n integer) RETURNS integer LANGUAGE SQL AS 'SELECT n'",
    )
    .unwrap()
    .remove(0) else {
        panic!("function declaration")
    };
    function.object_id = Some([60; 16]);
    function.owner = Some(RoleIdentity::BOOTSTRAP);
    function.params[0].default = Some(default);
    RetainedValue::make_mut(&mut catalog.definitions.sql_user_functions)
        .insert(
            "public.checked".into(),
            vec![SQLUserFunction::new(*function, RoutineBody::Source).shared()],
        )
        .unwrap();
    CatalogReadView::new(catalog)
}

fn cast(ty: &str) -> Expr {
    Expr::Cast {
        implicit: false,
        expr: Box::new(Expr::Literal(Value::Null)),
        ty: ty.into(),
    }
}

fn call(binding: FunctionBinding, args: Vec<Expr>) -> Expr {
    Expr::Func {
        name: "coalesce".into(),
        binding: Some(binding),
        args,
        distinct: false,
        order_by: Vec::new(),
        order_syntax: FunctionCallSyntax::Ordinary,
        filter: None,
    }
}

fn assert_work(catalog: &CatalogReadView, minimum: usize, expected: &str) {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let inline = validate(catalog.snapshot(), &memory, &token)
        .unwrap_err()
        .to_string();
    assert!(inline.contains(expected), "{inline}");
    let (single, result) = drain(catalog, 1);
    assert_eq!(result.unwrap_err().to_string(), inline);
    let (batched, result) = drain(catalog, 32);
    assert_eq!(result.unwrap_err().to_string(), inline);
    assert!(single > minimum);
    assert_eq!(batched, single.div_ceil(32));
    let mut cursor = start(catalog, &memory, &token);
    for _ in 0..minimum / 2 {
        assert!(poll(&mut cursor, 1).unwrap().is_none());
    }
    token.cancel();
    assert_eq!(poll(&mut cursor, 1).unwrap_err().sqlstate(), Some("57014"));
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_default_types_yield_for_native_syntax_and_selected_invocation_fields() {
    let width = COUNT as usize;
    let mut args = vec![Expr::Literal(Value::Int(1)); width];
    args.push(cast("enum#999999"));
    let binding = FunctionBinding::builtin_by_name("coalesce", None);
    assert_work(
        &catalog_with_default(call(binding.clone(), args)),
        width,
        "missing type or different type kind",
    );
    let mut arguments = binding.clone();
    arguments.argument_types = vec!["int4".into(); width];
    arguments.argument_types.push("enum#999999".into());
    assert_work(
        &catalog_with_default(call(arguments, Vec::new())),
        width,
        "missing type or different type kind",
    );
    let mut invocation = binding;
    invocation.invocation = Some(Box::new(RoutineInvocationBinding {
        argument_positions: Vec::new(),
        argument_targets: Vec::new(),
        argument_sources: vec![None; width],
        parameter_types: vec!["enum#999999".into()],
        return_type: Some("domain#bad".into()),
        variadic_mode: RoutineVariadicMode::None,
    }));
    assert_work(
        &catalog_with_default(call(invocation, Vec::new())),
        width,
        "missing type or different type kind",
    );
}

#[test]
fn stored_default_type_spelling_yields_for_whitespace_digits_and_array_suffixes() {
    let size = 128 << 10;
    for spelling in [
        format!("{}enum#bad", "\u{2003}".repeat(size)),
        format!("domain#{}x", "0".repeat(size)),
        format!("range#bad{}", "[]".repeat(size)),
    ] {
        assert_work(
            &catalog_with_default(cast(&spelling)),
            size / 1024,
            "malformed stored type identity",
        );
    }
}

#[test]
fn stored_default_type_errors_preserve_subquery_and_window_child_order() {
    let Statement::Select(mut select) =
        uqa_sql::compile("SELECT sum(1) FILTER (WHERE true) OVER (PARTITION BY 1)")
            .unwrap()
            .remove(0)
    else {
        panic!("select")
    };
    let mut window = select.projections.remove(0).expr;
    let Expr::WindowCall { filter, spec, .. } = &mut window else {
        panic!("window")
    };
    *filter = Some(Box::new(cast("enum#bad")));
    spec.partition_by[0] = cast("enum#999999");
    let Statement::Select(mut select) = uqa_sql::compile("SELECT (SELECT 1)").unwrap().remove(0)
    else {
        panic!("select")
    };
    let subquery = select.projections.remove(0).expr;
    for (expression, expected) in [
        (window, "malformed stored type identity"),
        (
            Expr::Array(vec![subquery, cast("enum#999999")]),
            "schema expression contains a subquery",
        ),
    ] {
        let catalog = catalog_with_default(expression);
        let error = drain(&catalog, 1).1.unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
}
