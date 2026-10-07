//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-plan lifetime and eligibility, against `PostgreSQL` 18 `inline_function`
//! and the independently captured `routine_parser_lifetime` oracle.

use super::*;
use crate::ast::Statement;
use std::sync::{atomic::Ordering, Arc};
use uqa_core::Value;
mod fixture;
use fixture::Catalog;

fn integer(value: i64) -> ScalarExpr {
    ScalarExpr::TypedLiteral {
        value: Value::Int(value),
        ty: "integer".into(),
        bound_type: Some(ColumnType::Integer),
        parameter_index: None,
    }
}
fn value(expression: &ScalarExpr) -> Option<&Value> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => Some(value),
        _ => None,
    }
}
fn call(binding: FunctionBinding, args: Vec<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args,
        distinct: false,
        order_by: vec![],
        filter: None,
        order_syntax: crate::ast::FunctionCallSyntax::Ordinary,
    }
}

#[test]
fn ordinary_planning_reparses_source_but_retained_replacements_keep_their_values() {
    let catalog =
        Catalog::new(r"CREATE FUNCTION fresh() RETURNS text LANGUAGE sql AS $$SELECT 'a\nb'$$");
    catalog.settings.lock().unwrap().standard_conforming_strings = false;
    let binding = catalog.binding("fresh", &[]);
    let first = catalog
        .context()
        .prepare(&binding, &[], &[])
        .unwrap()
        .unwrap();
    assert_eq!(value(&first.expression), Some(&Value::Str("a\nb".into())));
    assert_eq!(catalog.take_notices().len(), 1);
    let second = catalog
        .context()
        .prepare(&binding, &[], &[])
        .unwrap()
        .unwrap();
    assert_eq!(value(&second.expression), value(&first.expression));
    assert_eq!(catalog.take_notices().len(), 1);
    catalog.settings.lock().unwrap().standard_conforming_strings = true;
    let changed = catalog
        .context()
        .prepare(&binding, &[], &[])
        .unwrap()
        .unwrap();
    assert_eq!(
        value(&changed.expression),
        Some(&Value::Str(r"a\nb".into()))
    );
    assert_eq!(catalog.take_notices().len(), 0);
    assert_eq!(value(&first.expression), Some(&Value::Str("a\nb".into())));
    assert_eq!(catalog.evaluations.load(Ordering::SeqCst), 0);
}

#[test]
fn metadata_exclusions_precede_source_parse_but_multiple_statements_are_parsed() {
    let catalog = Catalog::new(
        r"CREATE FUNCTION configured() RETURNS text LANGUAGE sql SET search_path=public AS $$SELECT 'a\nb'$$;
        CREATE FUNCTION secured() RETURNS text LANGUAGE sql SECURITY DEFINER AS $$SELECT 'a\nb'$$;
        CREATE FUNCTION set_result() RETURNS SETOF text LANGUAGE sql AS $$SELECT 'a\nb'$$;
        CREATE FUNCTION multiple() RETURNS text LANGUAGE sql AS $$SELECT 1; SELECT 'a\nb'$$",
    );
    catalog.settings.lock().unwrap().standard_conforming_strings = false;
    for name in ["configured", "secured", "set_result"] {
        assert!(catalog
            .context()
            .prepare(&catalog.binding(name, &[]), &[], &[])
            .unwrap()
            .is_none());
    }
    assert_eq!(catalog.take_notices().len(), 0);
    for _ in 0..2 {
        assert!(catalog
            .context()
            .prepare(&catalog.binding("multiple", &[]), &[], &[])
            .unwrap()
            .is_none());
        assert_eq!(catalog.take_notices().len(), 1);
    }
}

#[test]
fn authority_recursion_and_strict_null_do_not_parse_source() {
    let mut catalog = Catalog::new(
        r"CREATE FUNCTION denied(x integer) RETURNS text LANGUAGE sql STRICT AS $$SELECT 'a\nb'$$",
    );
    Arc::make_mut(&mut catalog.functions[0]).def.execute_acl = Some(vec![]);
    catalog.allowed.store(false, Ordering::SeqCst);
    catalog.settings.lock().unwrap().standard_conforming_strings = false;
    let binding = catalog.binding("denied", &[Some(ColumnType::Integer)]);
    assert!(catalog
        .context()
        .prepare(&binding, &[integer(1)], &[])
        .unwrap()
        .is_none());
    let null = catalog
        .context()
        .prepare(&binding, &[ScalarExpr::Literal(Value::Null)], &[])
        .unwrap()
        .unwrap();
    assert_eq!(value(&null.expression), Some(&Value::Null));
    assert!(catalog
        .context()
        .prepare(&binding, &[integer(1)], &[binding.object_id.unwrap()])
        .unwrap()
        .is_none());
    assert_eq!(catalog.take_notices().len(), 0);
}

#[test]
fn immutable_constant_calls_use_existing_executor_before_inline_attempts() {
    let catalog = Catalog::new("CREATE FUNCTION folded(x integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $$SELECT x+1$$");
    let binding = catalog.binding("folded", &[Some(ColumnType::Integer)]);
    let folded = catalog
        .context()
        .prepare(&binding, &[integer(7)], &[])
        .unwrap()
        .unwrap();
    assert_eq!(value(&folded.expression), Some(&Value::Int(37)));
    assert_eq!(catalog.evaluations.load(Ordering::SeqCst), 1);
    assert_eq!(catalog.take_notices().len(), 0);
}

#[test]
fn typed_substitution_uses_invocation_positions_and_preserves_caller_parameters() {
    let catalog = Catalog::new("CREATE FUNCTION reordered(a integer,b integer DEFAULT 8) RETURNS integer LANGUAGE sql AS $$SELECT a-b$$");
    let mut binding = catalog.binding(
        "reordered",
        &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
    );
    binding
        .invocation
        .as_mut()
        .unwrap()
        .argument_positions
        .swap(0, 1);
    let replacement = catalog
        .context()
        .prepare(&binding, &[ScalarExpr::Param(2), integer(9)], &[])
        .unwrap()
        .unwrap();
    let ScalarExpr::Binary { lhs, rhs, .. } = replacement.expression else {
        panic!("expected subtraction")
    };
    assert_eq!(value(&lhs), Some(&Value::Int(9)));
    assert!(matches!(*rhs, ScalarExpr::Param(2)));
    let omitted = catalog.binding("reordered", &[Some(ColumnType::Integer)]);
    let replacement = catalog
        .context()
        .prepare(&omitted, &[integer(4)], &[])
        .unwrap()
        .unwrap();
    let ScalarExpr::Binary { rhs, .. } = replacement.expression else {
        panic!("expected subtraction")
    };
    assert!(matches!(
        *rhs,
        ScalarExpr::Cast { .. }
            | ScalarExpr::Literal(Value::Int(8))
            | ScalarExpr::TypedLiteral {
                value: Value::Int(8),
                ..
            }
    ));
}

#[test]
fn strictness_volatility_and_multiple_uses_preserve_effects() {
    let catalog = Catalog::new("CREATE FUNCTION twice(x bigint) RETURNS bigint LANGUAGE sql AS $$SELECT x+x$$;
        CREATE FUNCTION unused(x integer) RETURNS integer LANGUAGE sql STRICT AS $$SELECT 1$$;
        CREATE FUNCTION nonstrict(x integer) RETURNS integer LANGUAGE sql STRICT AS $$SELECT coalesce(x,1)$$;
        CREATE FUNCTION between_strict(x integer) RETURNS boolean LANGUAGE sql STRICT AS $$SELECT x BETWEEN 1 AND 3$$;
        CREATE FUNCTION in_strict(x integer) RETURNS boolean LANGUAGE sql STRICT AS $$SELECT x IN (1,2)$$;
        CREATE FUNCTION stable_body() RETURNS double precision LANGUAGE sql STABLE AS $$SELECT random()$$;
        CREATE FUNCTION expensive(x bigint) RETURNS bigint LANGUAGE sql COST 11 AS $$SELECT x$$");
    let volatile = crate::plan::ExpressionPlan::lower(
        crate::compile("SELECT nextval('seq')")
            .unwrap()
            .into_iter()
            .find_map(|statement| {
                if let Statement::Select(select) = statement {
                    Some(select.projections[0].expr.clone())
                } else {
                    None
                }
            })
            .unwrap(),
    )
    .scalar;
    let binding = catalog.binding("twice", &[Some(ColumnType::BigInteger)]);
    assert!(catalog
        .context()
        .prepare(&binding, &[volatile], &[])
        .unwrap()
        .is_none());
    let expensive = call(
        catalog.binding("expensive", &[Some(ColumnType::BigInteger)]),
        vec![ScalarExpr::Param(1)],
    );
    assert!(catalog
        .context()
        .prepare(&binding, &[expensive], &[])
        .unwrap()
        .is_none());
    for name in ["unused", "nonstrict", "between_strict", "in_strict"] {
        assert!(catalog
            .context()
            .prepare(
                &catalog.binding(name, &[Some(ColumnType::Integer)]),
                &[integer(1)],
                &[]
            )
            .unwrap()
            .is_none());
    }
    assert!(catalog
        .context()
        .prepare(&catalog.binding("stable_body", &[]), &[], &[])
        .unwrap()
        .is_none());
}

#[test]
fn retained_identity_and_return_coercion_survive_inline_analysis() {
    let catalog = Catalog::new("CREATE FUNCTION inner_value(x integer) RETURNS integer LANGUAGE sql AS $$SELECT x$$;
        CREATE FUNCTION outer_value(x integer) RETURNS bigint LANGUAGE sql AS $$SELECT inner_value(x)$$");
    let binding = catalog.binding("outer_value", &[Some(ColumnType::Integer)]);
    let replacement = catalog
        .context()
        .prepare(&binding, &[ScalarExpr::Param(3)], &[])
        .unwrap()
        .unwrap();
    let ScalarExpr::Cast { expr, ty, .. } = replacement.expression else {
        panic!("result assignment is required")
    };
    assert_eq!(
        ColumnType::from_sql_name(&ty).unwrap(),
        ColumnType::BigInteger
    );
    let ScalarExpr::Func {
        binding: Some(selected),
        args,
        ..
    } = *expr
    else {
        panic!("selected nested call")
    };
    assert_eq!(selected.object_id, catalog.functions[0].def.object_id);
    assert!(matches!(args.as_slice(), [ScalarExpr::Param(3)]));
}

#[test]
fn ordinary_cache_replanning_is_separate_from_the_execution_body_cache() {
    let catalog = Catalog::new("CREATE FUNCTION fresh() RETURNS integer LANGUAGE sql AS $$SELECT 1$$;
        CREATE FUNCTION configured() RETURNS integer LANGUAGE sql SET search_path=public AS $$SELECT 1$$");
    for (name, expected) in [("fresh", true), ("configured", false)] {
        let mut plan = UnifiedPlan::lower(crate::compile("SELECT 1").unwrap().remove(0));
        plan.rewrite_scalar_expressions(&mut |expression| {
            *expression = ScalarExpr::Cast {
                implicit: false,
                expr: Box::new(call(catalog.binding(name, &[]), vec![])),
                ty: "bigint".into(),
            };
        });
        assert_eq!(catalog.context().requires_replanning(&plan), expected);
    }
}

#[test]
fn materialized_variadic_arguments_keep_the_selected_array_signature() {
    let catalog = Catalog::new("CREATE FUNCTION packed(VARIADIC items integer[]) RETURNS integer[] LANGUAGE sql AS $$SELECT items$$");
    let binding = catalog.binding(
        "packed",
        &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
    );
    let (selected, arguments) = catalog
        .context()
        .materialize_call(&binding, &[integer(4), integer(7)])
        .unwrap()
        .unwrap();
    let signature = crate::function_call_argument_signature(
        &arguments,
        &crate::RowSchema::default(),
        &[],
        Some(&catalog),
    )
    .unwrap();
    assert!(signature.2);
    let resolved = crate::FunctionTypeResolver::resolve_function_type(
        &catalog,
        "packed",
        Some(&selected),
        &signature.0,
        &signature.1,
        signature.2,
    )
    .unwrap();
    assert_eq!(
        resolved,
        Some(ColumnType::Array(Box::new(ColumnType::Integer)))
    );
    let replacement = catalog
        .context()
        .prepare(&selected, &arguments, &[])
        .unwrap()
        .unwrap();
    assert!(matches!(replacement.expression, ScalarExpr::Cast { .. }));
}

#[test]
fn stable_operator_and_output_casts_prevent_immutable_body_expansion() {
    let catalog = Catalog::new("CREATE FUNCTION zone_add(x timestamptz) RETURNS timestamptz LANGUAGE sql IMMUTABLE AS $$SELECT x+interval '1 day'$$;
        CREATE FUNCTION zone_compare(x timestamptz) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$SELECT x=timestamp '2024-01-01'$$;
        CREATE FUNCTION zone_output(x timestamptz) RETURNS text LANGUAGE sql IMMUTABLE AS $$SELECT x::text$$;
        CREATE FUNCTION local_add(x timestamp) RETURNS timestamp LANGUAGE sql IMMUTABLE AS $$SELECT x+interval '1 day'$$");
    for name in ["zone_add", "zone_compare", "zone_output"] {
        assert!(
            catalog
                .context()
                .prepare(
                    &catalog.binding(name, &[Some(ColumnType::TimestampTz)]),
                    &[ScalarExpr::Param(1)],
                    &[]
                )
                .unwrap()
                .is_none(),
            "{name}"
        );
    }
    assert!(catalog
        .context()
        .prepare(
            &catalog.binding("local_add", &[Some(ColumnType::Timestamp)]),
            &[ScalarExpr::Param(1)],
            &[]
        )
        .unwrap()
        .is_some());
}

#[test]
fn expression_mutability_uses_its_own_column_types() {
    use crate::ast::FunctionVolatility::{Immutable, Stable};
    let catalog = Catalog::new("");
    for (ty, second, expression, expected) in [
        ("integer", "integer", "v + 1", Immutable),
        ("integer", "integer", "v::text", Immutable),
        ("timestamp", "integer", "v::timestamptz", Stable),
        ("timestamptz", "integer", "v::timestamp", Stable),
        ("timestamp", "interval", "v + w", Immutable),
        ("timestamptz", "interval", "v + w", Stable),
        ("timestamptz", "timestamp", "v < w", Stable),
        ("timestamptz", "timestamp", "v IN (w)", Stable),
        ("timestamptz", "timestamp", "v BETWEEN w AND w", Stable),
    ] {
        let Statement::Select(query) = crate::compile(&format!("SELECT {expression}"))
            .unwrap()
            .remove(0)
        else {
            panic!("scalar query");
        };
        let scalar = crate::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
        let schema = crate::RowSchema::with_types(
            vec!["v".into(), "w".into()],
            vec![
                Some(ColumnType::from_sql_name(ty).unwrap()),
                Some(ColumnType::from_sql_name(second).unwrap()),
            ],
        );
        let scalar = crate::type_resolution::bind_type_introspection_with_resolver(
            scalar,
            &schema,
            &[],
            &catalog,
        );
        assert_eq!(
            catalog
                .context()
                .expression_volatility(&scalar, &schema)
                .unwrap(),
            expected,
            "{ty}: {expression}"
        );
    }
}

#[test]
fn assigned_default_type_does_not_replace_the_default_expression_type() {
    for (declaration, target) in [
        (
            "CREATE FUNCTION f(v integer DEFAULT 1.6) RETURNS integer LANGUAGE SQL AS 'SELECT v'",
            ColumnType::Integer,
        ),
        (
            "CREATE FUNCTION f(v text DEFAULT 12) RETURNS text LANGUAGE SQL AS 'SELECT v'",
            ColumnType::Text,
        ),
    ] {
        let mut catalog = Catalog::new(declaration);
        Arc::make_mut(&mut catalog.functions[0]).def.params[0].default_type =
            Some(crate::ast::RoutineDefaultType::Concrete(target.clone()));
        let expanded = catalog
            .context()
            .prepare(&catalog.binding("f", &[]), &[], &[])
            .unwrap()
            .unwrap();
        assert_eq!(
            crate::scalar_type(&expanded.expression, &crate::RowSchema::default(), &[]).unwrap(),
            Some(target)
        );
        assert!(matches!(
            expanded.expression,
            ScalarExpr::Cast { implicit: true, .. }
        ));
    }
}
