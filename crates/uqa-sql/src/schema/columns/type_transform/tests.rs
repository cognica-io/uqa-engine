//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{FunctionBinding, FunctionVolatility};
use crate::schema::SchemaExpressionCatalog;
use crate::{expr::EngineHook, routines::RoutineResolution, FunctionTypeResolver};
use std::sync::Arc;
use uqa_core::Value;

struct Catalog;

impl FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.resolve_function_overload(name, binding, names, types, variadic)
            .map(|selected| selected.map(|selected| selected.return_type))
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        _: &[Option<String>],
        types: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<crate::type_resolution::ResolvedFunctionOverload>, SQLError> {
        Ok((matches!(name, "sum" | "public.sum")
            && types == [Some(ColumnType::Text)]
            && binding.is_none_or(|binding| binding.object_id == Some([7; 16])))
        .then(|| crate::type_resolution::ResolvedFunctionOverload {
            binding: FunctionBinding {
                object_id: Some([7; 16]),
                name: "public.sum".into(),
                argument_types: vec!["text".into()],
                builtin: false,
                dispatch: None,
                invocation: None,
                resolution_error: None,
            },
            return_type: ColumnType::Integer,
            exact_matches: 1,
            known_arguments: 1,
            preferred_matches: 0,
            precedes_pg_catalog: true,
        }))
    }

    fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
        Ok(binding.object_id == Some([7; 16]))
    }
}

impl RoutineResolution for Catalog {
    fn resolve_static_sql_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
    ) -> Result<Option<Arc<crate::routines::SQLUserFunction>>, SQLError> {
        if self
            .resolve_function_overload(name, binding, names, types, variadic)?
            .is_none()
        {
            return Ok(None);
        }
        let crate::Statement::CreateFunction(mut definition) = crate::compile(
            "CREATE FUNCTION public.sum(text) RETURNS integer LANGUAGE SQL IMMUTABLE AS 'SELECT 3'",
        )?
        .remove(0) else {
            unreachable!("fixture routine declaration");
        };
        definition.object_id = Some([7; 16]);
        Ok(Some(Arc::new(crate::routines::SQLUserFunction::new(
            *definition,
            crate::routines::RoutineBody::Source,
        ))))
    }
}

impl crate::catalog::domain::DomainCatalog for Catalog {
    fn domain_by_oid(&self, _: u32) -> Option<crate::catalog::domain::StoredDomain> {
        panic!("planning a base coercion does not inspect domain membership")
    }
}

impl crate::assignment::AssignmentContext for Catalog {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        _: &crate::ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        panic!("planning does not evaluate domain constraints")
    }
}

impl crate::plan::AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("transform analysis does not evaluate functions")
    }

    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("transform analysis does not evaluate functions")
    }

    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("transform analysis does not evaluate functions")
    }
}

impl SchemaExpressionCatalog for Catalog {
    fn registered_runtime_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }

    fn schema_expression_columns(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        panic!("transform analysis uses the supplied original columns")
    }
}

fn expression(sql: &str) -> Expr {
    let crate::Statement::AlterTable(statement) = crate::compile(&format!(
        "ALTER TABLE typecheck_empty ALTER COLUMN a TYPE integer USING {sql}"
    ))
    .unwrap()
    .remove(0) else {
        panic!("expected ALTER TABLE")
    };
    let crate::ast::AlterTableAction::AlterColumnType {
        using: Some(expression),
        ..
    } = statement.actions.into_iter().next().unwrap()
    else {
        panic!("expected ALTER COLUMN TYPE USING")
    };
    expression
}

fn columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef::nullable("a", ColumnType::Integer),
        ColumnDef::nullable("b", ColumnType::Integer),
        ColumnDef::nullable("t", ColumnType::Text),
    ]
}

fn analyze(
    context: &SchemaBindingContext<'_, '_>,
    source: &str,
) -> Result<AnalyzedTypeTransform, SQLError> {
    analyze_type_transform(
        context,
        "typecheck_empty",
        "typecheck_empty",
        &columns(),
        &expression(source),
    )
}

// PostgreSQL 18.4's transform-expression oracle distinguishes argument errors, call signatures and prohibited constructs in their analysis order.
#[test]
fn using_analysis_preserves_nested_diagnostic_order_without_rows() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    for (source, state, message) in [
        (
            "missing_column",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "sum(missing_column)",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "sum(absent_function(a))",
            "42883",
            "function absent_function(integer) does not exist",
        ),
        (
            "absent_function(sum(a))",
            "42803",
            "aggregate functions are not allowed in transform expressions",
        ),
        (
            "missing_column + sum(a)",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "sum(a) + missing_column",
            "42803",
            "aggregate functions are not allowed in transform expressions",
        ),
        (
            "pg_catalog.sum(a)",
            "42803",
            "aggregate functions are not allowed in transform expressions",
        ),
        (
            "abs((SELECT missing_column))",
            "0A000",
            "cannot use subquery in transform expression",
        ),
        ("sum(true)", "42883", "function sum(boolean) does not exist"),
        (
            "generate_series(missing_column, 2)",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "generate_series(1, 2)",
            "0A000",
            "set-returning functions are not allowed in transform expressions",
        ),
        (
            "absent_function(a) OVER ()",
            "42883",
            "function absent_function(integer) does not exist",
        ),
        (
            "row_number() OVER (ORDER BY missing_column)",
            "42P20",
            "window functions are not allowed in transform expressions",
        ),
        ("$1", "42P02", "there is no parameter $1"),
        (
            "other_table.a",
            "42P01",
            "missing FROM-clause entry for table \"other_table\"",
        ),
    ] {
        let error = match analyze(&context, source) {
            Ok(_) => panic!("{source} must fail analysis"),
            Err(error) => error,
        };
        assert_eq!(error.sqlstate(), Some(state), "{source}: {error}");
        assert_eq!(error.to_string(), message, "{source}");
        assert_eq!(error.detail(), None, "{source}");
        assert_eq!(
            error.hint(),
            (state == "42883").then_some("No function matches the given name and argument types. You might need to add explicit type casts."),
            "{source}",
        );
    }
}

#[test]
fn transform_analysis_uses_the_selected_scalar_overload_of_an_aggregate_name() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let transform = analyze(&context, "sum(t)").unwrap();
    assert_eq!(transform.source_type, Some(ColumnType::Integer));
    assert!(
        matches!(transform.plan.scalar, ScalarExpr::Func { binding: Some(binding), .. } if binding.object_id == Some([7; 16]))
    );
    let error = analyze(&context, "sum(DISTINCT t)").err().unwrap();
    assert_eq!(error.sqlstate(), Some("42809"));
    assert_eq!(
        error.to_string(),
        "DISTINCT specified, but sum is not an aggregate function"
    );
}

#[test]
fn transform_analysis_retains_the_fixed_repeat_signature_and_rejects_other_types() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let transform = analyze(&context, "repeat('x', 3)").unwrap();
    assert_eq!(transform.source_type, Some(ColumnType::Text));
    assert!(matches!(
        transform.plan.scalar,
        ScalarExpr::Func { binding: Some(binding), .. }
            if binding.builtin && binding.object_id.is_none()
                && binding.name == "pg_catalog.repeat"
                && binding.argument_types == ["text", "integer"]
    ));
    for source in ["repeat('x')", "repeat(1, 3)", "repeat('x', 3::bigint)"] {
        let error = analyze(&context, source).err().unwrap();
        assert_eq!(error.sqlstate(), Some("42883"), "{source}: {error}");
    }
}

#[test]
fn original_columns_and_all_target_qualifiers_keep_their_types() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    for source in [
        "a + b",
        "typecheck_empty.a + 1",
        "public.typecheck_empty.a + 1",
    ] {
        let transform = analyze(&context, source).unwrap();
        assert_eq!(transform.source_type, Some(ColumnType::Integer), "{source}");
        assert_eq!(
            transform.row_schema().type_of("a"),
            Some(&ColumnType::Integer)
        );
    }
    let transform = analyze(&context, "t").unwrap();
    assert_eq!(transform.source_type, Some(ColumnType::Text));
}

#[test]
fn assignment_checks_distinguish_using_from_implicit_conversion() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    for (explicit, message, hint) in [
        (
            true,
            "result of USING clause for column \"a\" cannot be cast automatically to type integer",
            "You might need to add an explicit cast.",
        ),
        (
            false,
            "column \"a\" cannot be cast automatically to type integer",
            "You might need to specify \"USING a::integer\".",
        ),
    ] {
        let mut transform = analyze(&context, "t").unwrap();
        let error = coerce_type_transform(
            &context,
            "a",
            &ColumnType::Integer,
            &mut transform,
            explicit,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        assert_eq!(error.to_string(), message);
        assert_eq!(error.detail(), None);
        assert_eq!(error.hint(), Some(hint));
    }
    let mut transform = analyze(&context, "t::integer").unwrap();
    coerce_type_transform(&context, "a", &ColumnType::Integer, &mut transform, true).unwrap();
}

#[test]
fn unknown_input_is_read_after_target_selection_and_null_is_retained() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let mut invalid = analyze(&context, "'bad'").unwrap();
    assert_eq!(invalid.source_type, None);
    let error =
        coerce_type_transform(&context, "a", &ColumnType::Integer, &mut invalid, true).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P02"));
    assert_eq!(
        error.to_string(),
        "invalid input syntax for type integer: \"bad\""
    );
    let mut valid = analyze(&context, "'42'").unwrap();
    coerce_type_transform(&context, "a", &ColumnType::Integer, &mut valid, true).unwrap();
    assert_eq!(valid.source_type, Some(ColumnType::Integer));
    assert!(matches!(
        valid.plan.scalar,
        ScalarExpr::TypedLiteral {
            value: Value::Int(42),
            ..
        }
    ));
    let mut null = analyze(&context, "NULL").unwrap();
    coerce_type_transform(&context, "a", &ColumnType::Integer, &mut null, true).unwrap();
    assert!(matches!(null.plan.scalar, ScalarExpr::Literal(Value::Null)));
}

#[test]
fn analysis_leaves_constant_evaluation_until_after_target_checks() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    for source in [
        "1 / 0",
        "CASE WHEN false THEN 1 / 0 ELSE a END",
        "CASE WHEN a > 0 THEN 1 ELSE 1 / 0 END",
        "COALESCE(1, 1 / 0)",
        "COALESCE(a, 1 / 0)",
    ] {
        let mut transform = analyze(&context, source).unwrap();
        coerce_type_transform(&context, "a", &ColumnType::Integer, &mut transform, true).unwrap();
        assert_eq!(transform.source_type, Some(ColumnType::Integer), "{source}");
        assert!(
            !matches!(transform.plan.scalar, ScalarExpr::Literal(_)),
            "{source}"
        );
    }
}

fn planned_constant(value: Value, source: ColumnType) -> AnalyzedTypeTransform {
    AnalyzedTypeTransform {
        plan: ExpressionPlan::lower(Expr::TypedLiteral {
            value,
            ty: source.sql_name(),
        }),
        source_type: Some(source),
        row_schema: RowSchema::default(),
    }
}

fn domain(base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "checked_value".into(),
        oid: 16384,
        array_oid: Some(16385),
        base: Box::new(base),
    }
}

// PostgreSQL 18.4 checks assignment range and typmods during planning even when there are no rows. These values represent the constants produced by scalar planning, not explicit target casts (which have different varchar truncation rules).
#[test]
fn planned_assignment_constants_check_ranges_and_typmods() {
    for (value, source, target, state, message) in [
        (
            Value::Int(i64::from(i32::MAX) + 1),
            ColumnType::BigInteger,
            ColumnType::Integer,
            "22003",
            "integer out of range",
        ),
        (
            Value::Int(1000),
            ColumnType::Integer,
            ColumnType::Numeric {
                precision: Some(2),
                scale: Some(0),
            },
            "22003",
            "numeric field overflow",
        ),
        (
            Value::Str("long".into()),
            ColumnType::Text,
            ColumnType::Varchar(Some(2)),
            "22001",
            "value too long for type character varying(2)",
        ),
    ] {
        let mut transform = planned_constant(value, source);
        let error = fold_type_transform_assignment(&Catalog, &target, &mut transform).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn constant_domain_base_coercions_defer_membership_and_array_coercions() {
    for value in [Value::Int(-1), Value::Null] {
        let mut transform = planned_constant(value.clone(), ColumnType::Integer);
        fold_type_transform_assignment(&Catalog, &domain(ColumnType::Integer), &mut transform)
            .unwrap();
        assert_eq!(transform.source_type, Some(ColumnType::Integer));
        assert!(
            matches!(transform.plan.scalar, ScalarExpr::TypedLiteral { value: actual, .. } if actual == value)
        );
    }
    let target = domain(ColumnType::Numeric {
        precision: Some(2),
        scale: Some(0),
    });
    let mut numeric = planned_constant(Value::Int(1000), ColumnType::Integer);
    assert_eq!(
        fold_type_transform_assignment(&Catalog, &target, &mut numeric)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
    let value = Value::Array(uqa_core::ArrayValue::try_new(vec![Value::Int(1000)]).unwrap());
    let source = ColumnType::Array(Box::new(ColumnType::Integer));
    let mut array = planned_constant(value.clone(), source.clone());
    fold_type_transform_assignment(&Catalog, &ColumnType::Array(Box::new(target)), &mut array)
        .unwrap();
    assert_eq!(array.source_type, Some(source));
    assert!(
        matches!(array.plan.scalar, ScalarExpr::TypedLiteral { value: actual, .. } if actual == value)
    );
}

#[test]
fn using_assignment_and_constant_folding_preserve_oid_source_width() {
    assert_eq!(
        assign_type_transform_value(
            &Catalog,
            Value::Int(-1),
            &ColumnType::Oid,
            Some(&ColumnType::SmallInteger),
        )
        .unwrap(),
        Value::Int(i64::from(u32::MAX))
    );
    let mut small = planned_constant(Value::Int(-1), ColumnType::SmallInteger);
    fold_type_transform_assignment(&Catalog, &ColumnType::Oid, &mut small).unwrap();
    assert!(
        matches!(small.plan.scalar, ScalarExpr::TypedLiteral { value: Value::Int(value), bound_type: Some(ColumnType::Oid), .. } if value == i64::from(u32::MAX))
    );
    let mut big = planned_constant(Value::Int(-1), ColumnType::BigInteger);
    assert_eq!(
        fold_type_transform_assignment(&Catalog, &ColumnType::Oid, &mut big)
            .unwrap_err()
            .sqlstate(),
        Some("22003")
    );
}

#[test]
fn embedding_declaration_migration_keeps_value_shape_validation() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let mut transform = analyze_type_transform(
        &context,
        "embeddings",
        "embeddings",
        &[ColumnDef::nullable("embedding", ColumnType::Vector(2))],
        &Expr::Column("embedding".into()),
    )
    .unwrap();
    let target = ColumnType::Tensor(3);
    coerce_type_transform(&context, "embedding", &target, &mut transform, false).unwrap();
    assert!(!crate::type_resolution::assignment_type_compatible(
        &ColumnType::Vector(2),
        &target
    ));
    assert!(assign_type_transform_value(
        &Catalog,
        Value::List(vec![Value::Float(1.0), Value::Float(0.0)]),
        &target,
        transform.source_type.as_ref(),
    )
    .is_err());
}
