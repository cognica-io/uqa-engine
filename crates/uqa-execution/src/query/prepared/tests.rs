//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_execute_parameters, ArgumentBindingContext, PreparedArgumentScopes,
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
    ColumnType, ResultRow, RowSchema, SQLError, SQLParam,
};

#[derive(Default)]
struct Scopes {
    events: RefCell<Vec<String>>,
    domain: Option<StoredDomain>,
}
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
        row: &ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        self.record("domain");
        Ok(Value::Bool(
            matches!(row["value"], Value::Int(value) if value > 0),
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
        let aggregates = |_: &str| false;
        let cast_type = |_: &str| None;
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
    bind_execute_parameters(scopes, "p", Some(types), &arguments(sql), &[])
}

#[test]
fn missing_parameterless_and_wrong_arity_definitions_do_not_capture_argument_scope() {
    let scopes = Scopes::default();
    let invalid = arguments("EXECUTE p((SELECT 1))");
    let error = bind_execute_parameters(&scopes, "p", None, &invalid, &[]).unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "26000"));
    assert!(
        bind_execute_parameters(&scopes, "p", Some(Vec::new()), &invalid, &[])
            .unwrap()
            .is_empty()
    );
    let error =
        bind_execute_parameters(&scopes, "p", Some(vec![None, None]), &invalid, &[]).unwrap_err();
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
        identity: uqa_core::RelationIdentity {
            schema: "public".into(),
            name: "positive".into(),
        },
        owner: "owner".into(),
        definition,
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
