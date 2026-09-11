//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::arguments::{
    immutable_argument, validate_argument, validate_assignment_type, ArgumentValidationContext,
};
use super::{declared_parameter_types, is_unknown_type, prepared_result_schema_matches};
use crate::semantics::volatility::VolatilityCatalog;
use crate::{
    ast::FunctionVolatility,
    plan::{CommandPlan, ExpressionPlan, UnifiedPlan},
    ScalarExpr,
};
use crate::{ColumnType, RowSchema, SQLError};

fn argument(sql: &str) -> ExpressionPlan {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("expected EXECUTE")
    };
    let CommandPlan::Execute { mut params, .. } = *command else {
        panic!("expected EXECUTE")
    };
    params.remove(0)
}
fn domain(oid: u32, name: &str) -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: name.into(),
        oid,
        base: Box::new(ColumnType::Integer),
    }
}

#[test]
fn prepared_descriptors_preserve_domain_identity_and_type_modifiers() {
    let schema = |name: &str, ty| RowSchema::with_types(vec![name.into()], vec![ty]);
    let left = schema("value", Some(domain(90_001, "original")));
    assert!(prepared_result_schema_matches(
        Some(&left),
        Some(&schema("value", Some(domain(90_001, "renamed"))))
    ));
    for changed in [
        schema("value", Some(domain(90_002, "original"))),
        schema("value", Some(ColumnType::Integer)),
        schema("renamed", Some(domain(90_001, "original"))),
        schema("value", None),
    ] {
        assert!(!prepared_result_schema_matches(Some(&left), Some(&changed)));
    }
    assert!(!prepared_result_schema_matches(
        Some(&schema("value", Some(ColumnType::Varchar(Some(10))))),
        Some(&schema("value", Some(ColumnType::Varchar(Some(11)))))
    ));
    assert!(prepared_result_schema_matches(None, None));
    assert!(!prepared_result_schema_matches(
        None,
        Some(&RowSchema::default())
    ));
    assert!(!prepared_result_schema_matches(
        Some(&RowSchema::default()),
        None
    ));
}

#[test]
fn unknown_declarations_keep_parameter_holes_and_resolve_qualified_types() {
    struct Types(std::sync::Mutex<Vec<String>>);
    impl crate::FunctionTypeResolver for Types {
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            panic!("parameter declarations must not resolve functions")
        }
        fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
            self.0.lock().unwrap().push(name.into());
            Ok(Some(ColumnType::Text))
        }
    }
    let types = Types(std::sync::Mutex::new(Vec::new()));
    let mut plan = UnifiedPlan::lower(crate::compile("SELECT $4").unwrap().remove(0));
    let declared = [
        ColumnType::Named("unknown".into()),
        ColumnType::Named("pg_catalog.unknown".into()),
        ColumnType::Named("app.unknown".into()),
    ];
    assert_eq!(
        declared_parameter_types(&types, &mut plan, &declared).unwrap(),
        [None, None, Some(ColumnType::Text), None]
    );
    assert_eq!(*types.0.lock().unwrap(), ["app.unknown"]);
    assert!(!is_unknown_type("unknown[]").unwrap());
    assert!(!is_unknown_type("unknown(3)").unwrap());
}

#[test]
fn execute_argument_shape_errors_precede_aggregate_catalog_lookup() {
    let lookups = std::cell::Cell::new(0);
    let aggregates = |name: &str| {
        lookups.set(lookups.get() + 1);
        name == "custom_aggregate"
    };
    for (sql, state) in [
        ("EXECUTE p((SELECT 1) + sum(1) OVER ())", "0A000"),
        ("EXECUTE p(sum(1) OVER ())", "42P20"),
        ("EXECUTE p(custom_aggregate(1))", "42803"),
    ] {
        let error = validate_argument(&aggregates, &argument(sql)).unwrap_err();
        assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == state));
        if state != "42803" {
            assert_eq!(lookups.get(), 0);
        }
    }
    assert!(lookups.get() > 0);
    assert!(validate_assignment_type(0, &ColumnType::Integer, &ColumnType::BigInteger).is_ok());
    let error =
        validate_assignment_type(2, &ColumnType::Boolean, &ColumnType::Integer).unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "42804" && message.starts_with("parameter $3 "))
    );
}

#[test]
fn execute_constant_classification_keeps_domains_and_runtime_inputs_deferred() {
    struct Catalog;
    impl VolatilityCatalog for Catalog {
        fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
            (name == "runtime_value").then_some(FunctionVolatility::Volatile)
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            None
        }
        fn view_query(&self, _: &str) -> Result<Option<crate::plan::QueryPlan>, SQLError> {
            Ok(None)
        }
    }
    let aggregates = |_: &str| false;
    let cast_type = |name: &str| {
        let parsed = crate::parse_regtype_name(name).ok().flatten()?;
        if parsed.names.last()?.as_str() != "positive" {
            return None;
        }
        let ty = domain(90_001, "positive");
        Some(if parsed.array_dimensions == 0 {
            ty
        } else {
            ColumnType::Array(Box::new(ty))
        })
    };
    let context = ArgumentValidationContext {
        aggregates: &aggregates,
        volatility: &Catalog,
        cast_type: &cast_type,
    };
    assert!(immutable_argument(
        &context,
        &argument("EXECUTE p(1 + 2)").scalar
    ));
    for sql in [
        "EXECUTE p($1 + 2)",
        "EXECUTE p(runtime_value())",
        "EXECUTE p(1::positive)",
        "EXECUTE p(ARRAY[1]::positive[])",
    ] {
        assert!(
            !immutable_argument(&context, &argument(sql).scalar),
            "{sql}"
        );
    }
    assert!(!immutable_argument(
        &context,
        &ScalarExpr::Column("value".into())
    ));
}
