//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct XmlDomain<'a> {
    checks: Cell<usize>,
    cancel: Option<&'a CancellationToken>,
    panic_after_check: bool,
}

impl EngineHook for XmlDomain<'_> {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
}

impl DomainCatalog for XmlDomain<'_> {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        let mut domain = Context::new().domain_by_oid(20_004)?;
        domain.definition.base = ColumnType::Xml;
        domain.definition.checks[0].name = Some("xml_check".into());
        (oid == 20_004).then_some(domain)
    }
}

impl AssignmentContext for XmlDomain<'_> {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        row: &dyn crate::expr::RowLookup,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        self.checks.set(self.checks.get() + 1);
        assert!(matches!(row.column("value"), Some(Value::Str(_))));
        assert!(
            !self.panic_after_check,
            "injected native domain callback panic"
        );
        if let Some(token) = self.cancel {
            token.cancel();
        }
        Ok(Value::Bool(true))
    }
}

#[test]
fn xml_domain_parser_and_retained_result_share_one_allowance() {
    let context = XmlDomain {
        checks: Cell::new(0),
        cancel: None,
        panic_after_check: false,
    };
    let ty = context.domain_by_oid(20_004).unwrap().column_type();
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let small = Value::Str("<a/>".into());
    let large = Value::Str("<n/>".repeat(2000));
    let original = cast_domain_value_with_control(&context, &small, Some("unknown"), &ty, &control)
        .unwrap()
        .unwrap();
    let retained = budget.used();
    assert!(retained > 0);
    assert_eq!(retained, original.reserved_bytes());
    for _ in 0..8 {
        let error =
            cast_domain_value_with_control(&context, &large, Some("unknown"), &ty, &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), retained);
        assert_eq!(context.checks.get(), 1);
        assert_eq!(*original, small);
    }
    drop(original);
    assert_eq!(budget.used(), 0);
    drop(cast_domain_value_with_control(&context, &small, Some("unknown"), &ty, &control).unwrap());
    assert_eq!(context.checks.get(), 2);
    assert_eq!(budget.used(), 0);
}

#[test]
fn domain_callback_cancellation_releases_input_and_catalog_handoffs() {
    for original_cancels in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let context = XmlDomain {
            checks: Cell::new(0),
            cancel: Some(if original_cancels {
                &original
            } else {
                &invoking
            }),
            panic_after_check: false,
        };
        let ty = context.domain_by_oid(20_004).unwrap().column_type();
        let budget = MemoryBudget::new(64 << 10);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let value = Value::Str("<a/>".into());
        assert_eq!(
            cast_domain_value_with_control(&context, &value, Some("unknown"), &ty, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(context.checks.get(), 1);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn domain_callback_unwind_releases_its_input_without_invalidating_previous_values() {
    let context = XmlDomain {
        checks: Cell::new(0),
        cancel: None,
        panic_after_check: true,
    };
    let ty = context.domain_by_oid(20_004).unwrap().column_type();
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = control.copy_value(&Value::Str("<a/>".into())).unwrap();
    let retained = budget.used();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cast_domain_value_with_control(&context, &value, Some("unknown"), &ty, &control)
    }));
    assert!(result.is_err());
    assert_eq!(context.checks.get(), 1);
    assert_eq!(*value, Value::Str("<a/>".into()));
    assert_eq!(budget.used(), retained);
    drop(value);
    assert_eq!(budget.used(), 0);
}
