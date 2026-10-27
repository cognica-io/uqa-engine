//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_execute_parameters, bind_host_parameters, ArgumentBindingContext, PreparedArgumentScopes,
    ScopedArgumentOperation,
};
use crate::scalar::plan::PhysicalEvalContext;
use std::cell::RefCell;
use uqa_core::Value;
use uqa_sql::{
    assignment::AssignmentContext,
    ast::{Expr, FunctionBinding, FunctionVolatility, Statement},
    catalog::domain::{DomainCatalog, StoredDomain},
    expr::EngineHook,
    plan::{CommandPlan, ExpressionPlan, QueryPlan, UnifiedPlan},
    prepared::arguments::ArgumentValidationContext,
    semantics::volatility::VolatilityCatalog,
    ColumnType, RowSchema, SQLError, SQLParam,
};

#[derive(Default)]
struct Scopes {
    events: RefCell<Vec<String>>,
    parameter_counts: RefCell<Vec<usize>>,
    domain: Option<StoredDomain>,
}

struct Types(Option<ColumnType>);
impl uqa_sql::FunctionTypeResolver for Types {
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
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(self
            .0
            .as_ref()
            .filter(|ty| ty.user_type_identity().as_deref() == Some(name))
            .cloned())
    }
}
impl uqa_sql::routines::RoutineResolution for Types {}
impl Scopes {
    fn record(&self, event: &str) {
        self.events.borrow_mut().push(event.into());
    }
}
impl EngineHook for Scopes {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence operation")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence operation")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("unexpected sequence operation")
    }
    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value, SQLError>> {
        match name {
            "runtime_effect" => {
                self.record("effect");
                Some(Ok(Value::Int(1)))
            }
            "constant_value" => {
                self.record("constant");
                Some(Ok(Value::Int(7)))
            }
            _ => None,
        }
    }
    fn resolve_regtype_output(&self, _: &ColumnType, _: i64) -> Result<Option<String>, String> {
        Ok(Some("positive".into()))
    }
}
impl DomainCatalog for Scopes {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        self.domain
            .as_ref()
            .filter(|domain| domain.oid == oid)
            .cloned()
    }
}
impl AssignmentContext for Scopes {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        row: &dyn uqa_sql::expr::RowLookup,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        self.record("domain");
        Ok(Value::Bool(
            matches!(row.column("value"), Some(Value::Int(value)) if *value > 0),
        ))
    }
}
impl VolatilityCatalog for Scopes {
    fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        match name {
            "runtime_effect" => Some(FunctionVolatility::Volatile),
            "constant_value" => Some(FunctionVolatility::Immutable),
            _ => None,
        }
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
}
impl PreparedArgumentScopes for Scopes {
    fn with_scope(
        &self,
        parameters: &[SQLParam],
        operation: &mut ScopedArgumentOperation<'_>,
    ) -> Result<Vec<SQLParam>, SQLError> {
        self.record("scope");
        self.parameter_counts.borrow_mut().push(parameters.len());
        let aggregates = |_: &str| false;
        let cast_type = |_: &str| None;
        let routines = Types(self.domain.as_ref().map(StoredDomain::column_type));
        let mut analyze_type = |argument: &ExpressionPlan| {
            let label = match &argument.scalar {
                uqa_sql::ScalarExpr::Func { name, .. } => name.as_str(),
                _ => "expression",
            };
            self.record(&format!("analyze:{label}"));
            Ok(Some(ColumnType::Integer))
        };
        operation(ArgumentBindingContext {
            validation: ArgumentValidationContext {
                aggregates: &aggregates,
                volatility: self,
                cast_type: &cast_type,
                routines: &routines,
                parameters,
            },
            assignment: self,
            analyze_type: &mut analyze_type,
            evaluation: PhysicalEvalContext::new(None, parameters).with_function_hook(self),
        })
    }
}
fn arguments(sql: &str) -> Vec<ExpressionPlan> {
    let UnifiedPlan::Command(command) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("expected EXECUTE")
    };
    let CommandPlan::Execute { params, .. } = *command else {
        panic!("expected EXECUTE")
    };
    params
}
fn bind(
    scopes: &Scopes,
    sql: &str,
    types: Vec<Option<ColumnType>>,
) -> Result<Vec<SQLParam>, SQLError> {
    bind_execute_parameters(scopes, "p", Some(types), &arguments(sql))
}

#[test]
fn sql_execute_and_host_binding_capture_distinct_parameter_scopes() {
    let scopes = Scopes::default();
    assert!(bind_execute_parameters(
        &scopes,
        "p",
        Some(vec![Some(ColumnType::Integer)]),
        &arguments("EXECUTE p($1)"),
    )
    .is_err());
    assert_eq!(*scopes.parameter_counts.borrow(), [0]);
    let bound = bind_host_parameters(
        &scopes,
        "p",
        Some(vec![Some(ColumnType::Integer)]),
        &[SQLParam::Scalar(Value::Int(7))],
    )
    .unwrap();
    assert_eq!(*scopes.parameter_counts.borrow(), [0, 1]);
    assert!(matches!(
        &bound[0],
        SQLParam::TypedScalar {
            value: Value::Int(7),
            ty: ColumnType::Integer,
        }
    ));
}

#[test]
fn removed_prepared_declarations_reject_null_before_argument_evaluation() {
    let scopes = Scopes::default();
    let ty = ColumnType::Enum(uqa_sql::ast::EnumTypeReference {
        schema: "public".into(),
        name: "removed".into(),
        oid: 42000,
        array_oid: 42001,
    });
    let errors = [
        bind(&scopes, "EXECUTE p(NULL)", vec![Some(ty.clone())]).unwrap_err(),
        bind_host_parameters(
            &scopes,
            "p",
            Some(vec![Some(ty)]),
            &[SQLParam::Scalar(Value::Null)],
        )
        .unwrap_err(),
    ];
    for error in errors {
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(error.to_string(), "cache lookup failed for type 42000");
    }
    assert_eq!(*scopes.events.borrow(), ["scope", "scope"]);
}

#[test]
fn missing_parameterless_and_wrong_arity_definitions_do_not_capture_argument_scope() {
    let scopes = Scopes::default();
    let invalid = arguments("EXECUTE p((SELECT 1))");
    let error = bind_execute_parameters(&scopes, "p", None, &invalid).unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "26000"));
    assert!(
        bind_execute_parameters(&scopes, "p", Some(Vec::new()), &invalid)
            .unwrap()
            .is_empty()
    );
    let error =
        bind_execute_parameters(&scopes, "p", Some(vec![None, None]), &invalid).unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "42601"));
    assert!(scopes.events.borrow().is_empty());
}

#[test]
fn argument_input_and_constant_failures_precede_volatile_execution() {
    for sql in [
        "EXECUTE p(runtime_effect(), 'invalid integer')",
        "EXECUTE p(runtime_effect(), 1 / 0)",
    ] {
        let scopes = Scopes::default();
        assert!(bind(&scopes, sql, vec![Some(ColumnType::Integer); 2]).is_err());
        let events = scopes.events.borrow();
        assert_eq!(events[0], "scope");
        assert_eq!(events[1], "analyze:runtime_effect");
        assert!(!events.iter().any(|event| event == "effect"));
    }
}

#[test]
fn all_argument_analysis_finishes_before_constant_then_volatile_evaluation() {
    let scopes = Scopes::default();
    let values = bind(
        &scopes,
        "EXECUTE p(runtime_effect(), constant_value(), runtime_effect())",
        vec![Some(ColumnType::Integer); 3],
    )
    .unwrap();
    assert_eq!(
        *scopes.events.borrow(),
        [
            "scope",
            "analyze:runtime_effect",
            "analyze:constant_value",
            "analyze:runtime_effect",
            "constant",
            "effect",
            "effect"
        ]
    );
    for (parameter, expected) in values.iter().zip([1, 7, 1]) {
        assert!(
            matches!(parameter, SQLParam::TypedScalar { value: Value::Int(value), ty: ColumnType::Integer } if *value == expected)
        );
    }
}

#[test]
fn domain_constraints_run_at_their_argument_position_after_base_input_conversion() {
    let Statement::CreateDomain(definition) = uqa_sql::compile(
        "CREATE DOMAIN positive AS integer CONSTRAINT positive_value CHECK (VALUE > 0)",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected domain")
    };
    let domain = StoredDomain {
        object_id: [1; 16],
        oid: 90_001,
        array_oid: None,
        identity: uqa_core::RelationIdentity {
            schema: "public".into(),
            name: "positive".into(),
        },
        owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        definition,
        array_name: None,
        usage_acl: None,
    };
    let target = domain.column_type();
    let scopes = Scopes {
        domain: Some(domain),
        ..Scopes::default()
    };
    let error = bind(
        &scopes,
        "EXECUTE p(runtime_effect(), '-1')",
        vec![Some(ColumnType::Integer), Some(target.clone())],
    )
    .unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "23514"));
    assert_eq!(
        *scopes.events.borrow(),
        ["scope", "analyze:runtime_effect", "effect", "domain"]
    );
    scopes.events.borrow_mut().clear();
    assert!(bind(
        &scopes,
        "EXECUTE p(runtime_effect(), 'invalid integer')",
        vec![Some(ColumnType::Integer), Some(target)]
    )
    .is_err());
    assert_eq!(*scopes.events.borrow(), ["scope", "analyze:runtime_effect"]);
}
