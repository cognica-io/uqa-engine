//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    assignment::AssignmentContext,
    catalog::domain::{DomainCatalog, StoredDomain},
    expr::EngineHook,
    ResultRow, RowSchema,
};
use std::cell::Cell;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Context {
    domain: StoredDomain,
    checks: Cell<usize>,
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
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        Ok((name == "public.legacy_items").then(|| self.domain.column_type()))
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
        _: &crate::ast::Expr,
        _: &ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        self.checks.set(self.checks.get() + 1);
        Ok(Value::Bool(true))
    }
}

fn context(base: ColumnType) -> Context {
    Context {
        checks: Cell::new(0),
        domain: StoredDomain {
            object_id: [3; 16],
            oid: 12345,
            identity: uqa_core::RelationIdentity::new("public", "legacy_items"),
            owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
            definition: crate::ast::CreateDomain {
                name: "public.legacy_items".into(),
                base,
                collation: None,
                default: None,
                not_null: None,
                checks: vec![crate::ast::DomainCheck {
                    name: Some("accepted".into()),
                    catalog_identity: None,
                    expression: crate::ast::Expr::Literal(Value::Bool(true)),
                }],
            },
        },
    }
}

#[test]
fn same_domain_identity_normalizes_carriers_without_rechecking_membership() {
    for base in [ColumnType::Int2Vector, ColumnType::OidVector] {
        let context = context(base.clone());
        let ty = context.domain.column_type();
        let old = Value::List(vec![Value::Int(1), Value::Int(2)]);
        let expected =
            crate::expr::cast_value(&old, super::super::column_type_name(&base)).unwrap();
        assert_eq!(
            crate::assignment::conversion::coerce_assignment_value(
                &context,
                old.clone(),
                &ty,
                Some(&ty)
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            crate::assignment::domain::cast_domain_value(
                &context,
                &old,
                Some("public.legacy_items"),
                &ty
            )
            .unwrap(),
            Some(expected.clone())
        );
        assert_eq!(context.checks.get(), 0);
        assert_eq!(
            crate::assignment::domain::assign_domain_value(&context, &old, &ty).unwrap(),
            Some(expected)
        );
        assert_eq!(context.checks.get(), 1);
    }
}

#[test]
fn predecessor_vectors_and_nested_arrays_normalize_once_under_the_original_allowance() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for base in [ColumnType::Int2Vector, ColumnType::OidVector] {
        for old in [
            Value::List(Vec::new()),
            Value::List(vec![Value::Int(1)]),
            Value::Array(ArrayValue::try_new(vec![Value::Int(1)]).unwrap()),
        ] {
            let normalized = normalize_legacy_vector_carrier_with_control(&old, &base, &control)
                .unwrap()
                .unwrap();
            assert!(matches!(&*normalized, Value::LegacyVector(_)));
            assert_eq!(normalized.array_view().unwrap().lower_bounds(), &[0]);
            assert!(
                normalize_legacy_vector_carrier_with_control(&normalized, &base, &control)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(memory.used(), normalized.reserved_bytes());
            drop(normalized);
            assert_eq!(memory.used(), 0);
        }
        let ty = ColumnType::Array(Box::new(context(base).domain.column_type()));
        let old = Value::Array(
            ArrayValue::with_lower_bounds(
                vec![
                    Value::List(vec![Value::Int(1), Value::Int(2)]),
                    Value::List(vec![Value::Int(3), Value::Int(4)]),
                ],
                vec![-2, 1],
            )
            .unwrap(),
        );
        let normalized = normalize_legacy_vector_carrier_with_control(&old, &ty, &control)
            .unwrap()
            .unwrap();
        let array = normalized.array_view().unwrap();
        assert_eq!(array.dimensions(), &[2]);
        assert_eq!(array.lower_bounds(), &[-2]);
        assert!(array
            .elements()
            .iter()
            .all(|value| matches!(value, Value::LegacyVector(_))));
        assert!(
            normalize_legacy_vector_carrier_with_control(&normalized, &ty, &control)
                .unwrap()
                .is_none()
        );
        drop(normalized);
        assert_eq!(memory.used(), 0);
        token.check().unwrap();
    }
    let ordinary = Value::List(vec![Value::Int(1), Value::Int(2)]);
    assert!(
        normalize_legacy_vector_carrier_with_control(&ordinary, &ColumnType::JsonB, &control)
            .unwrap()
            .is_none()
    );
    token.cancel();
    assert_eq!(
        normalize_legacy_vector_carrier_with_control(&ordinary, &ColumnType::Int2Vector, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
}
