//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    assignment::AssignmentContext,
    ast::{CreateDomain, DomainCheck, Expr},
    catalog::domain::{DomainCatalog, StoredDomain},
    expr::EngineHook,
    ResultRow, RowSchema,
};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken, RelationIdentity};

mod retained_invocation;

struct Context {
    domain: StoredDomain,
    resolutions: Cell<usize>,
    checks: Cell<usize>,
    accept: Cell<bool>,
    after_check: std::cell::RefCell<Option<Box<dyn Fn()>>>,
}

impl Context {
    fn new() -> Self {
        Self {
            domain: StoredDomain {
                object_id: [8; 16],
                oid: 12345,
                array_oid: None,
                identity: RelationIdentity::new("public", "accepted_value"),
                owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
                definition: CreateDomain {
                    collation_oid: None,
                    name: "public.accepted_value".into(),
                    base: ColumnType::Integer,
                    collation: None,
                    default: None,
                    not_null: None,
                    checks: vec![DomainCheck {
                        name: Some("accepted".into()),
                        catalog_identity: None,
                        expression: Expr::Literal(Value::Bool(true)),
                        validated: true,
                    }],
                },
                array_name: None,
                usage_acl: None,
            },
            resolutions: Cell::new(0),
            checks: Cell::new(0),
            accept: Cell::new(true),
            after_check: std::cell::RefCell::new(None),
        }
    }
}

impl EngineHook for Context {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn resolve_regtype_output(&self, _: &ColumnType, oid: i64) -> Result<Option<String>, String> {
        Ok((oid == i64::from(self.domain.oid)).then(|| "accepted_value".into()))
    }
}

impl DomainCatalog for Context {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        (oid == self.domain.oid).then(|| self.domain.clone())
    }
}

impl AssignmentContext for Context {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        _: &ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        self.checks.set(self.checks.get() + 1);
        if let Some(callback) = self.after_check.borrow().as_ref() {
            callback();
        }
        Ok(Value::Bool(self.accept.get()))
    }
}

impl RoutineValueContext for Context {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        self.resolutions.set(self.resolutions.get() + 1);
        (name == "public.accepted_value")
            .then(|| self.domain.column_type())
            .or_else(|| ColumnType::from_sql_name(name).ok())
    }
}

#[test]
fn routine_descriptions_keep_domain_checks_live_and_preserve_membership() {
    let context = Context::new();
    let description = RoutineValueDescription::resolve(&context, "public.accepted_value").unwrap();
    for value in 0..32 {
        assert_eq!(
            description
                .coerce(&context, &Value::Int(value), None)
                .unwrap(),
            Value::Int(value)
        );
    }
    assert_eq!(context.resolutions.get(), 1);
    assert_eq!(context.checks.get(), 32);
    context.accept.set(false);
    let error = description
        .coerce(&context, &Value::Int(33), None)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    assert_eq!(
        error.to_string(),
        "value for domain accepted_value violates check constraint \"accepted\""
    );
    assert_eq!(context.checks.get(), 33);
    // A value that already carries this domain's identity retains its established membership.
    assert_eq!(
        description
            .coerce(
                &context,
                &Value::Int(34),
                Some(&context.domain.column_type())
            )
            .unwrap(),
        Value::Int(34)
    );
    assert_eq!(context.checks.get(), 33);
}

#[test]
fn routine_pseudotype_descriptions_keep_values_and_reject_each_invalid_input() {
    let context = Context::new();
    for name in ["record", "trigger", "anyarray", "refcursor", "void"] {
        let description = RoutineValueDescription::resolve(&context, name).unwrap();
        assert_eq!(
            description.coerce(&context, &Value::Null, None).unwrap(),
            Value::Null
        );
        assert_eq!(
            description
                .coerce(&context, &Value::Int(7), None)
                .unwrap_err()
                .sqlstate(),
            Some("42804")
        );
    }
    let record = Value::Record(
        vec![
            ("same".into(), Value::Int(1)),
            ("same".into(), Value::Int(2)),
        ]
        .into(),
    );
    let description = RoutineValueDescription::resolve(&context, "pg_catalog.record").unwrap();
    assert_eq!(description.coerce(&context, &record, None).unwrap(), record);
    assert_eq!(context.resolutions.get(), 0);
}

#[test]
fn retained_routine_descriptions_admit_owned_payload_and_release_on_failure() {
    let context = Context::new();
    let description = RoutineValueDescription::resolve(&context, "public.accepted_value").unwrap();
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = uqa_core::memory::ProductionControl::new(&budget, &cancellation, &cancellation);
    let retained = description.clone_with_control(&control).unwrap();
    assert!(budget.used() >= size_of::<ColumnType>() + "publicaccepted_value".len());
    drop(retained);
    assert_eq!(budget.used(), 0);
    let tiny = MemoryBudget::new(1);
    let control = uqa_core::memory::ProductionControl::new(&tiny, &cancellation, &cancellation);
    assert!(description.clone_with_control(&control).is_err());
    assert_eq!(tiny.used(), 0);
    cancellation.cancel();
    assert!(description.clone_with_control(&control).is_err());
    assert_eq!(tiny.used(), 0);
}
