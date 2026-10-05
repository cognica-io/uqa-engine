//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AlterDomainAction, ColumnDef, FunctionBinding, FunctionVolatility, Statement};
use crate::schema::SchemaExpressionCatalog;
use crate::{expr::EngineHook, routines::RoutineResolution, FunctionTypeResolver};

struct Catalog;

impl FunctionTypeResolver for Catalog {
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

impl RoutineResolution for Catalog {}

impl crate::plan::AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("CHECK analysis does not execute expressions");
    }

    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("CHECK analysis does not execute expressions");
    }

    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("CHECK analysis does not execute expressions");
    }
}

impl SchemaExpressionCatalog for Catalog {
    fn registered_runtime_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        None
    }

    fn schema_expression_columns(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        panic!("a domain CHECK has only VALUE");
    }
}

fn definition(sql: &str) -> CreateDomain {
    let Statement::CreateDomain(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("CREATE DOMAIN");
    };
    definition
}

fn check(sql: &str) -> DomainCheck {
    let Statement::AlterDomain(alter) = crate::compile(&format!("ALTER DOMAIN d ADD {sql}"))
        .unwrap()
        .remove(0)
    else {
        panic!("ALTER DOMAIN");
    };
    let AlterDomainAction::AddCheck { constraint } = alter.action else {
        panic!("ADD CHECK");
    };
    *constraint
}

#[test]
fn added_check_analysis_preserves_postgresql_expression_error_order() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let definition = definition("CREATE DOMAIN public.d AS integer");
    // PostgreSQL 18.4 analyzes each child before its call and rejects window placement before its window specification.
    for (source, state, message) in [
        (
            "1",
            "42804",
            "argument of CHECK must be type boolean, not type integer",
        ),
        (
            "sum(missing_column)>0",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "sum(true)>0",
            "42883",
            "function sum(boolean) does not exist",
        ),
        (
            "sum(VALUE)>0 AND missing_column",
            "42803",
            "aggregate functions are not allowed in check constraints",
        ),
        (
            "missing_column AND sum(VALUE)>0",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "pg_catalog.sum(VALUE)>0",
            "42803",
            "aggregate functions are not allowed in check constraints",
        ),
        (
            "row_number() OVER (ORDER BY missing_column)>0",
            "42P20",
            "window functions are not allowed in check constraints",
        ),
        (
            "sum(missing_column) OVER ()>0",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "generate_series(1,2)>0 AND missing_column",
            "0A000",
            "set-returning functions are not allowed in check constraints",
        ),
        (
            "generate_series(missing_column,2)>0",
            "42703",
            "column \"missing_column\" does not exist",
        ),
        (
            "other_table.value > 0",
            "42P01",
            "missing FROM-clause entry for table \"other_table\"",
        ),
        (
            "(SELECT missing_column)",
            "0A000",
            "cannot use subquery in check constraint",
        ),
        (
            "'invalid'",
            "22P02",
            "invalid input syntax for type boolean: \"invalid\"",
        ),
        ("$1", "42P02", "there is no parameter $1"),
    ] {
        let error = prepare_added_check(
            &context,
            &definition,
            check(&format!("CHECK ({source})")),
            &BTreeSet::new(),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{source}: {error}");
        assert_eq!(error.to_string(), message, "{source}");
    }
}

#[test]
fn added_check_binds_only_the_new_expression_and_retains_validation_state() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let definition = definition(
        "CREATE DOMAIN public.d AS integer DEFAULT unavailable_function() CONSTRAINT old_check CHECK (unavailable_column)",
    );
    let original = serde_json::to_value(&definition).unwrap();
    let check = prepare_added_check(
        &context,
        &definition,
        check("CHECK (abs(VALUE) > 0) NOT VALID"),
        &BTreeSet::from(["d_check".into()]),
    )
    .unwrap();
    assert_eq!(check.name.as_deref(), Some("d_check1"));
    assert!(!check.validated);
    assert!(check.catalog_identity.is_none());
    let Expr::Binary { lhs, .. } = check.expression else {
        panic!("comparison");
    };
    assert!(matches!(lhs.as_ref(), Expr::Func { binding: Some(binding), .. } if binding.builtin));
    assert_eq!(serde_json::to_value(definition).unwrap(), original);
}

#[test]
fn duplicate_constraint_name_precedes_binding_the_new_check() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let definition =
        definition("CREATE DOMAIN public.d AS integer CONSTRAINT positive CHECK (VALUE>0)");
    let error = prepare_added_check(
        &context,
        &definition,
        check("CONSTRAINT positive CHECK (missing_column)"),
        &BTreeSet::new(),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42710"));
    assert_eq!(
        error.to_string(),
        "constraint \"positive\" for domain \"d\" already exists"
    );
    let error = prepare_added_not_null(
        &definition,
        DomainNotNull {
            name: Some("positive".into()),
            catalog_identity: None,
        },
        &BTreeSet::new(),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42710"));
}

#[test]
fn unknown_check_literals_are_read_as_boolean_and_existing_not_null_is_retained() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = SchemaBindingContext {
        catalog: &Catalog,
        binding: &binding,
    };
    let mut definition =
        definition("CREATE DOMAIN public.d AS integer CONSTRAINT required NOT NULL");
    let identity = crate::ast::ConstraintCatalogIdentity {
        object_id: [7; 16],
        oid: 50_001,
    };
    definition.not_null.as_mut().unwrap().catalog_identity = Some(identity);
    for (source, expected) in [("'true'", Value::Bool(true)), ("NULL", Value::Null)] {
        let check = prepare_added_check(
            &context,
            &definition,
            check(&format!("CHECK ({source})")),
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(
            matches!(check.expression, Expr::TypedLiteral { value, ty } if value == expected && ty == "boolean")
        );
    }
    let constraint = prepare_added_not_null(
        &definition,
        DomainNotNull {
            name: Some("another_name".into()),
            catalog_identity: None,
        },
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(constraint.name.as_deref(), Some("required"));
    assert_eq!(constraint.catalog_identity, Some(identity));
}
