//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

use super::*;
use crate::ast::EnumFunctionOperation as Operation;
use crate::expr::enums::{enum_function_value, EnumTypeLabel, EnumTypeLabels};
use uqa_core::{DatumValue, EnumLabelKey, EnumValue};

fn physical(oid: u32) -> Value {
    Value::Datum(DatumValue::new(16_384, 0, oid.to_le_bytes().to_vec()))
}

fn nested(kind: usize, first: Value, second: Value) -> Value {
    match kind {
        0 => Value::Array(uqa_core::ArrayValue::try_new(vec![first, second]).unwrap()),
        1 => Value::Record(vec![("e".into(), first), ("n".into(), second)]),
        2 => Value::Row(vec![first, second].into()),
        _ => unreachable!(),
    }
}

#[test]
fn internal_group_order_agrees_with_enum_identity_and_skips_unreached_fields() {
    use crate::expr::{
        compare_grouping_values_with_enum_catalog as compare,
        equal_typed_values_with_enum_catalog as equal,
    };
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let catalog = Catalog::new();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for kind in 0..3 {
        let values = [1, 3, 5].map(|oid| nested(kind, physical(oid), Value::Null));
        for (i, left) in values.iter().enumerate() {
            for (j, right) in values.iter().enumerate() {
                let order = compare(left, right, &control, Some(&catalog)).unwrap();
                assert_eq!(order, i.cmp(&j));
                assert_eq!(
                    order.is_eq(),
                    equal(left, right, &control, Some(&catalog)).unwrap()
                );
            }
        }
        let malformed = Value::Datum(DatumValue::new(16_384, 0, vec![1]));
        let left = nested(kind, physical(1), malformed.clone());
        let right = nested(kind, physical(3), malformed);
        assert_eq!(
            compare(&left, &right, &control, Some(&catalog)).unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare(&left, &left, &control, Some(&catalog))
                .unwrap_err()
                .sqlstate(),
            Some("XX001")
        );
    }
    token.cancel();
    assert_eq!(
        compare(&physical(1), &physical(1), &control, Some(&catalog))
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn nested_enum_equality_preserves_raw_identity_nulls_and_lazy_fields() {
    use crate::expr::binary::eval_comparison_truth_with_enum_catalog as compare;
    use uqa_core::{memory::MemoryBudget, memory::ProductionControl, CancellationToken};

    let catalog = Catalog::new();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let malformed = Value::Datum(DatumValue::new(16_384, 0, vec![1]));
    for kind in 0..3 {
        let apply = |op, left: &Value, right: &Value| {
            compare(op, left, right, &control, Some(&catalog), None)
        };
        let same = nested(kind, physical(1), Value::Null);
        let different = nested(kind, physical(3), Value::Null);
        assert_eq!(
            apply(BinaryOp::Equal, &same, &same).unwrap(),
            (kind != 2).then_some(true)
        );
        assert_eq!(
            apply(BinaryOp::Equal, &same, &different).unwrap(),
            Some(false)
        );
        assert_eq!(
            apply(BinaryOp::NotEqual, &same, &different).unwrap(),
            Some(true)
        );
        let left = nested(kind, physical(1), malformed.clone());
        let right = nested(kind, physical(3), malformed.clone());
        assert_eq!(apply(BinaryOp::Equal, &left, &right).unwrap(), Some(false));
        assert_eq!(
            apply(BinaryOp::Equal, &left, &left).unwrap_err().sqlstate(),
            Some("XX001")
        );
        assert_eq!(budget.used(), 0);
    }
    let record_field = nested(2, nested(2, physical(1), Value::Null), Value::Int(0));
    assert_eq!(
        compare(
            BinaryOp::Equal,
            &record_field,
            &record_field,
            &control,
            Some(&catalog),
            None
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        compare(
            BinaryOp::Less,
            &record_field,
            &nested(2, nested(2, physical(1), Value::Int(1)), Value::Int(0)),
            &control,
            Some(&catalog),
            None
        )
        .unwrap(),
        Some(false)
    );
    token.cancel();
    assert_eq!(
        compare(
            BinaryOp::Equal,
            &physical(1),
            &physical(1),
            &control,
            Some(&catalog),
            None
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
}

#[test]
fn syntax_nested_enum_equality_covers_borrowed_and_evaluated_operands() {
    let catalog = Catalog::new();
    let context = crate::expr::EvalContext::new(None, &[]).with_engine(&catalog);
    for kind in 0..3 {
        let operand = crate::ast::Expr::Literal(nested(kind, physical(1), Value::Int(0)));
        for left in [
            operand.clone(),
            crate::ast::Expr::Case {
                base: None,
                when: vec![(
                    crate::ast::Expr::Literal(Value::Bool(true)),
                    operand.clone(),
                )],
                else_branch: None,
            },
        ] {
            let expression = crate::ast::Expr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(left),
                rhs: Box::new(operand.clone()),
            };
            assert_eq!(
                crate::expr::eval(&expression, &context).unwrap(),
                Value::Bool(true)
            );
        }
    }
}

#[test]
fn scalar_operators_use_oid_identity_and_the_same_order_cache() {
    let catalog = Catalog::new();
    let state = EnumComparisonState::default();
    let apply = |op, left, right| eval_comparison(op, &left, &right, Some(&catalog), Some(&state));
    assert_eq!(
        apply(BinaryOp::Equal, physical(1), physical(1)).unwrap(),
        Some(Value::Bool(true))
    );
    assert_eq!(
        apply(BinaryOp::NotEqual, physical(1), physical(3)).unwrap(),
        Some(Value::Bool(true))
    );
    assert_eq!(
        apply(BinaryOp::Less, physical(2), physical(4)).unwrap(),
        Some(Value::Bool(true))
    );
    assert_eq!(
        apply(BinaryOp::GreaterEqual, physical(1), physical(1)).unwrap(),
        Some(Value::Bool(true))
    );
    assert!(state.cached_type().is_none());
    assert_eq!(
        apply(BinaryOp::Less, physical(5), physical(4)).unwrap(),
        Some(Value::Bool(true))
    );
    assert_eq!(state.cached_type(), Some(16_384));
    let error = apply(BinaryOp::Less, physical(11), physical(8)).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error.to_string().contains("enum_probe"));
    assert_eq!(
        apply(BinaryOp::Equal, physical(11), physical(11)).unwrap(),
        Some(Value::Bool(true))
    );
}

#[test]
fn scalar_enum_dispatch_preserves_nulls_bounds_and_other_operators() {
    let catalog = Catalog::new();
    let malformed = Value::Datum(DatumValue::new(16_384, 0, vec![1]));
    assert_eq!(
        eval_comparison(
            BinaryOp::Less,
            &malformed,
            &Value::Null,
            Some(&catalog),
            None
        )
        .unwrap(),
        Some(Value::Null)
    );
    assert_eq!(
        eval_comparison(
            BinaryOp::Equal,
            &malformed,
            &physical(1),
            Some(&catalog),
            None
        )
        .unwrap_err()
        .sqlstate(),
        Some("XX001")
    );
    assert_eq!(
        eval_comparison(
            BinaryOp::Less,
            &physical(1),
            &physical(3),
            Some(&catalog),
            None
        )
        .unwrap_err()
        .sqlstate(),
        Some("22P03")
    );
    assert_eq!(
        eval_comparison(
            BinaryOp::Less,
            &Value::Int(1),
            &Value::Int(3),
            Some(&catalog),
            None
        )
        .unwrap(),
        None
    );
    assert_eq!(
        eval_comparison(
            BinaryOp::Add,
            &malformed,
            &physical(1),
            Some(&catalog),
            None
        )
        .unwrap(),
        None
    );
}

#[test]
fn invalid_label_identity_still_supports_equality_even_order_and_hashes() {
    let apply = |operation, arguments: &[Value]| {
        enum_function_value(None, operation, 16_384, arguments).unwrap()
    };
    assert_eq!(
        apply(Operation::Equal, &[physical(1), physical(1)]),
        Value::Bool(true)
    );
    assert_eq!(
        apply(Operation::Compare, &[physical(1), physical(1)]),
        Value::Int(0)
    );
    assert_eq!(
        apply(Operation::Compare, &[physical(2), physical(4)]),
        Value::Int(-1)
    );
    assert_eq!(
        apply(Operation::Hash, &[physical(1)]),
        Value::Int(-1_905_060_026)
    );
    assert_eq!(
        apply(Operation::ExtendedHash, &[physical(1), Value::Int(0)]),
        Value::Int(-3_670_598_878_359_251_130)
    );
    let selected = apply(Operation::Smaller, &[physical(2), physical(4)]);
    assert!(selected.has_same_representation(&physical(2)));
    assert_eq!(
        apply(Operation::Hash, &[selected]),
        Value::Int(1_134_484_726)
    );
    assert_eq!(
        apply(Operation::Compare, &[physical(1), Value::Null]),
        Value::Null
    );
}

#[test]
fn legacy_native_keys_keep_catalog_free_equality_and_order() {
    let keys = EnumLabelKey::initial(2).unwrap();
    let values = keys
        .into_iter()
        .map(|key| Value::Enum(EnumValue::new(16_384, key)))
        .collect::<Vec<_>>();
    assert_eq!(
        enum_function_value(None, Operation::Compare, 16_384, &values).unwrap(),
        Value::Int(-1)
    );
    assert_eq!(
        enum_function_value(None, Operation::Equal, 16_384, &values).unwrap(),
        Value::Bool(false)
    );
}

struct Catalog(
    Arc<EnumTypeLabels>,
    std::sync::atomic::AtomicI64,
    EnumTypeComparisonStates,
);

impl Catalog {
    fn new() -> Self {
        Self(
            Arc::new(EnumTypeLabels {
                type_oid: 16_384,
                labels: EnumLabelKey::initial(3)
                    .unwrap()
                    .into_iter()
                    .zip([(2, "first"), (5, "middle"), (4, "last")])
                    .map(|(key, (oid, label))| EnumTypeLabel {
                        oid,
                        key,
                        label: label.into(),
                    })
                    .collect(),
            }),
            std::sync::atomic::AtomicI64::new(0),
            EnumTypeComparisonStates::default(),
        )
    }
}

impl EnumLabelCatalog for Catalog {
    fn enum_type_comparison_states(&self) -> Option<&EnumTypeComparisonStates> {
        Some(&self.2)
    }

    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>> {
        Ok((oid == self.0.type_oid).then(|| Arc::clone(&self.0)))
    }
    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>> {
        if matches!(oid, 8 | 11) {
            return Ok(Some((16_400, usize::from(oid == 8))));
        }
        Ok(self
            .0
            .labels
            .iter()
            .position(|label| label.oid == oid)
            .map(|position| (self.0.type_oid, position)))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, oid: u32) -> Result<Option<String>> {
        Ok(Some(
            if oid == 16_400 {
                "second_enum"
            } else {
                "enum_probe"
            }
            .into(),
        ))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

#[test]
fn nested_enum_order_shares_type_support_but_keeps_row_fields_private() {
    use crate::expr::binary::eval_comparison_truth_with_enum_catalog as compare;
    use uqa_core::memory::ProductionControl;

    let catalog = Catalog::new();
    let control = ProductionControl::uncontrolled();
    let state = EnumComparisonState::default();
    let less = |left: Value, right: Value| {
        compare(
            BinaryOp::Less,
            &left,
            &right,
            &control,
            Some(&catalog),
            Some(&state),
        )
    };
    assert_eq!(
        less(
            nested(0, physical(5), Value::Null),
            nested(0, physical(4), Value::Null)
        )
        .unwrap(),
        Some(true)
    );
    let error = less(
        nested(1, physical(11), Value::Null),
        nested(1, physical(8), Value::Null),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error.to_string().contains("enum_probe"));
    assert_eq!(
        less(
            nested(0, physical(4), Value::Null),
            nested(0, physical(8), Value::Null)
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        less(
            nested(2, physical(5), physical(11)),
            nested(2, physical(5), physical(8))
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        less(
            nested(2, physical(5), physical(11)),
            nested(2, physical(4), physical(8))
        )
        .unwrap(),
        Some(true)
    );
    assert_eq!(state.cached_type(), None);
    assert_eq!(state.field(0).cached_type(), Some(16_384));
    assert_eq!(state.field(1).cached_type(), Some(16_400));
    let retained = catalog.2.get(16_384);
    assert_eq!(retained.cached_type(), Some(16_384));
    for _ in 0..10 {
        assert!(Arc::ptr_eq(&retained, &catalog.2.get(16_384)));
    }
    let fresh = Catalog::new();
    assert_eq!(fresh.2.get(16_384).cached_type(), None);
}

#[test]
fn nested_enum_order_preserves_lazy_null_and_error_observations() {
    use crate::expr::binary::eval_comparison_truth_with_enum_catalog as compare;
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };

    let catalog = Catalog::new();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let bad = Value::Datum(DatumValue::new(16_384, 0, vec![1]));
    let less = |left: Value, right: Value| {
        compare(
            BinaryOp::Less,
            &left,
            &right,
            &control,
            Some(&catalog),
            None,
        )
    };
    for kind in 0..3 {
        let left = nested(kind, physical(2), bad.clone());
        let right = nested(kind, physical(4), bad.clone());
        assert_eq!(less(left.clone(), right).unwrap(), Some(true));
        assert_eq!(
            less(left.clone(), left).unwrap_err().sqlstate(),
            Some("XX001")
        );
        assert_eq!(
            less(
                nested(kind, Value::Null, bad.clone()),
                nested(kind, physical(4), bad.clone())
            )
            .unwrap(),
            (kind != 2).then_some(false)
        );
    }
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert_eq!(
        less(physical(1), physical(1)).unwrap_err().sqlstate(),
        Some("57014")
    );
}

impl crate::expr::EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64> {
        Ok(self.1.fetch_add(1, AtomicOrdering::Relaxed) + 1)
    }
    fn currval(&self, _: &str) -> Result<i64> {
        Ok(self.1.load(AtomicOrdering::Relaxed))
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn enum_labels(&self) -> Option<&dyn EnumLabelCatalog> {
        Some(self)
    }
}

#[test]
fn syntax_evaluation_compares_physical_enum_operands_without_output() {
    let catalog = Catalog::new();
    let context = crate::expr::EvalContext::new(None, &[]).with_engine(&catalog);
    let expression = |op, left, right| crate::ast::Expr::Binary {
        op,
        lhs: Box::new(crate::ast::Expr::Literal(physical(left))),
        rhs: Box::new(crate::ast::Expr::Literal(physical(right))),
    };
    assert_eq!(
        crate::expr::eval(&expression(BinaryOp::Less, 5, 4), &context).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        crate::expr::eval(&expression(BinaryOp::Equal, 1, 1), &context).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn comparison_cache_follows_the_first_slow_call_and_bypasses_fast_calls() {
    let catalog = Catalog::new();
    let state = EnumComparisonState::default();
    let compare = |left, right| {
        super::compare(
            Some(&catalog),
            &physical(left),
            &physical(right),
            Some(&state),
        )
    };
    assert_eq!(compare(1, 1).unwrap(), Ordering::Equal);
    assert_eq!(compare(2, 8).unwrap(), Ordering::Less);
    assert_eq!(state.cached_type(), None);
    assert_eq!(compare(11, 8).unwrap(), Ordering::Less);
    assert_eq!(state.cached_type(), Some(16_400));
    assert_eq!(compare(5, 5).unwrap(), Ordering::Equal);
    assert_eq!(compare(4, 8).unwrap(), Ordering::Less);
    let error = compare(5, 4).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error
        .to_string()
        .contains("enum value 5 not found in cache for enum second_enum"));
    assert_eq!(compare(1, 2).unwrap_err().sqlstate(), Some("XX000"));
    let fresh = EnumComparisonState::default();
    let error =
        super::compare(Some(&catalog), &physical(1), &physical(2), Some(&fresh)).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P03"));
    assert_eq!(fresh.cached_type(), None);
}

#[test]
fn admitted_native_values_use_the_call_sites_existing_actual_type() {
    let catalog = Catalog::new();
    let state = EnumComparisonState::default();
    super::compare(Some(&catalog), &physical(5), &physical(4), Some(&state)).unwrap();
    let keys = EnumLabelKey::initial(2).unwrap();
    let left = Value::Enum(EnumValue::new(16_400, keys[0].clone()).with_label_oid(Some(11)));
    let right = Value::Enum(EnumValue::new(16_400, keys[1].clone()).with_label_oid(Some(8)));
    let error = super::compare(Some(&catalog), &left, &right, Some(&state)).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error
        .to_string()
        .contains("enum value 11 not found in cache for enum enum_probe"));
    assert_eq!(
        super::compare(
            Some(&catalog),
            &left,
            &right,
            Some(&EnumComparisonState::default())
        )
        .unwrap(),
        Ordering::Less,
    );
}

#[test]
fn bound_key_equality_uses_raw_enum_identities_inside_containers() {
    let catalog = Catalog::new();
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let native = Value::Enum(
        EnumValue::new(16_384, catalog.0.labels[1].key.clone()).with_label_oid(Some(5)),
    );
    let equal = |left: &Value, right: &Value| {
        crate::expr::equal_typed_values_with_enum_catalog(left, right, &control, Some(&catalog))
    };
    for (left, right) in [(physical(5), native), (physical(1), physical(1))] {
        assert!(equal(&left, &right).unwrap());
        let left = Value::Array(uqa_core::ArrayValue::try_new(vec![left, Value::Null]).unwrap());
        let right = Value::Array(uqa_core::ArrayValue::try_new(vec![right, Value::Null]).unwrap());
        assert!(equal(&left, &right).unwrap());
        assert!(equal(
            &Value::Record(vec![("a".into(), left)]),
            &Value::Record(vec![("a".into(), right)])
        )
        .unwrap());
    }
    assert!(!equal(&physical(1), &physical(5)).unwrap());
    assert!(!equal(&physical(1), &Value::Null).unwrap());
    let corrupt = Value::Datum(DatumValue::new(16_384, 0, Vec::new()));
    assert!(!equal(&corrupt, &Value::Null).unwrap());
    assert!(equal(&corrupt, &physical(1)).is_err());
}

#[test]
fn odd_order_reads_actual_label_order_and_preserves_both_error_boundaries() {
    let catalog = Catalog::new();
    assert_eq!(
        compare(Some(&catalog), &physical(5), &physical(4), None).unwrap(),
        Ordering::Less
    );
    for (left, right, expected_state, expected_message) in [
        (1, 2, "22P03", "invalid internal value for enum: 1"),
        (
            2,
            1,
            "XX000",
            "enum value 1 not found in cache for enum enum_probe",
        ),
    ] {
        let error = compare(Some(&catalog), &physical(left), &physical(right), None).unwrap_err();
        let SQLError::Routine { sqlstate, message } = error else {
            panic!("typed comparison diagnostic")
        };
        assert_eq!(sqlstate, expected_state);
        assert_eq!(message, expected_message);
    }
}

#[test]
fn type_only_functions_do_not_read_bytes_and_bounds_match_label_oids() {
    let catalog = Catalog::new();
    let apply = |operation, arguments: &[Value]| {
        enum_function_value(Some(&catalog), operation, 16_384, arguments).unwrap()
    };
    let unread = Value::Datum(DatumValue::new(16_384, u32::MAX, Vec::new()));
    assert_eq!(
        oid(
            None,
            &apply(Operation::First, std::slice::from_ref(&unread))
        )
        .unwrap(),
        Some(2)
    );
    assert_eq!(
        oid(None, &apply(Operation::Last, std::slice::from_ref(&unread))).unwrap(),
        Some(4)
    );
    let Value::Array(all) = apply(Operation::Range, &[unread]) else {
        panic!("enum range")
    };
    assert_eq!(all.elements().len(), 3);
    for (lower, upper, expected) in [
        (physical(1), Value::Null, vec![]),
        (Value::Null, physical(1), vec![2, 5, 4]),
        (physical(0), physical(0), vec![2, 5, 4]),
        (physical(5), physical(4), vec![5, 4]),
        (physical(4), physical(2), vec![]),
    ] {
        let Value::Array(range) = apply(Operation::BoundedRange, &[lower, upper]) else {
            panic!("enum range")
        };
        let actual = range
            .elements()
            .iter()
            .map(|value| oid(None, value).unwrap().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}

fn syntax_call(
    name: &str,
    args: Vec<crate::ast::Expr>,
    dispatch: Option<crate::ast::FunctionDispatch>,
) -> crate::ast::Expr {
    crate::ast::Expr::Func {
        name: name.into(),
        binding: dispatch.map(crate::ast::FunctionBinding::dispatched),
        args,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
    }
}

#[test]
fn syntax_between_preserves_repeated_values_and_lazy_bounds() {
    use crate::ast::{Expr, FunctionDispatch};
    let catalog = Catalog::new();
    let context = crate::expr::EvalContext::new(None, &[]).with_engine(&catalog);
    let next = || {
        syntax_call(
            "nextval",
            vec![Expr::Literal(Value::Str("seq".into()))],
            None,
        )
    };
    let number = |value| Expr::Literal(Value::Int(value));
    let between = Expr::Between {
        expr: Box::new(next()),
        low: Box::new(number(0)),
        high: Box::new(number(1)),
    };
    assert_eq!(
        crate::expr::eval(&between, &context).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(catalog.1.load(AtomicOrdering::Relaxed), 2);
    catalog.1.store(0, AtomicOrdering::Relaxed);
    let between = Expr::Between {
        expr: Box::new(number(0)),
        low: Box::new(number(1)),
        high: Box::new(next()),
    };
    assert_eq!(
        crate::expr::eval(&between, &context).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(catalog.1.load(AtomicOrdering::Relaxed), 0);
    let symmetric = syntax_call(
        "between_symmetric",
        vec![next(), number(1), number(1)],
        Some(FunctionDispatch::BetweenSymmetric),
    );
    assert_eq!(
        crate::expr::eval(&symmetric, &context).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(catalog.1.load(AtomicOrdering::Relaxed), 4);
}

#[test]
fn syntax_membership_and_selection_observe_enum_ids_without_output() {
    use crate::ast::Expr;
    let catalog = Catalog::new();
    let context = crate::expr::EvalContext::new(None, &[]).with_engine(&catalog);
    let value = |oid| Expr::Literal(physical(oid));
    let membership = Expr::InList {
        expr: Box::new(value(1)),
        list: vec![value(3), value(1)],
        negated: false,
    };
    assert_eq!(
        crate::expr::eval(&membership, &context).unwrap(),
        Value::Bool(true)
    );
    let nullif = syntax_call("nullif", vec![value(1), value(1)], None);
    assert_eq!(crate::expr::eval(&nullif, &context).unwrap(), Value::Null);
    let greatest = syntax_call("greatest", vec![value(5), value(4)], None);
    assert!(crate::expr::eval(&greatest, &context)
        .unwrap()
        .has_same_representation(&physical(4)));
}
