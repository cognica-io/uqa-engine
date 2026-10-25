//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::FunctionBinding;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Builtins;

impl FunctionTypeResolver for Builtins {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        unreachable!()
    }
}

impl EngineHook for Builtins {
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

fn integer(value: i64) -> SQLParam {
    SQLParam::typed_scalar(Value::Int(value), ColumnType::Integer)
}

fn bounds<'a>(lower: &'a SQLParam, upper: &'a SQLParam) -> RangeParameterBounds<'a> {
    RangeParameterBounds::Bounds {
        lower: Some(lower),
        upper: Some(upper),
        lower_inclusive: false,
        upper_inclusive: true,
    }
}

#[test]
fn typed_bounds_normalization_and_observation_retain_the_original_allowance() {
    let budget = MemoryBudget::new(32768);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let lower = integer(1);
    let upper = integer(4);
    let value = create_range_parameter_with_control(
        "int4range",
        bounds(&lower, &upper),
        &Builtins,
        &Builtins,
        &control,
    )
    .unwrap();
    assert_eq!(value.scalar_value(), Some(&Value::Str("[2,5)".into())));
    assert!(budget.used() > 0);
    let empty = create_range_parameter_with_control(
        "int4range",
        RangeParameterBounds::Empty,
        &Builtins,
        &Builtins,
        &control,
    )
    .unwrap();
    let members = [(*empty).clone(), (*value).clone(), (*value).clone()];
    let multi = create_multirange_parameter_with_control(
        "int4multirange",
        &members,
        &Builtins,
        &Builtins,
        &control,
    )
    .unwrap();
    assert_eq!(multi.scalar_value(), Some(&Value::Str("{[2,5)}".into())));
    let ranges = crate::expr::RangeComparisonValue::from_type_with_control(
        multi.scalar_value().unwrap(),
        multi.declared_scalar_type().unwrap(),
        None,
        &control,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ranges.member_count(), Some(1));
    assert!(ranges.bounds().is_none());
    let member = ranges
        .member_value_with_control(0, None, &control)
        .unwrap()
        .unwrap();
    assert_eq!(*member, Value::Str("[2,5)".into()));
    assert!(ranges
        .member_value_with_control(1, None, &control)
        .unwrap()
        .is_none());
    drop((member, ranges, multi, empty, value));
    assert_eq!(budget.used(), 0);
}

#[test]
fn bound_failures_and_cancellation_release_every_partial_constructor() {
    for allowance in [0, 64, 256, 1024, 16384] {
        let budget = MemoryBudget::new(allowance);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let lower = integer(4);
        let upper = integer(1);
        let error = create_range_parameter_with_control(
            "int4range",
            bounds(&lower, &upper),
            &Builtins,
            &Builtins,
            &control,
        )
        .unwrap_err();
        assert!(matches!(error.sqlstate(), Some("22000" | "53200")));
        assert_eq!(budget.used(), 0);
        let null = SQLParam::Scalar(Value::Null);
        let error = create_multirange_parameter_with_control(
            "int4multirange",
            &[null],
            &Builtins,
            &Builtins,
            &control,
        )
        .unwrap_err();
        assert!(matches!(error.sqlstate(), Some("22004" | "53200")));
        assert_eq!(budget.used(), 0);
        for token in [&original, &invoking] {
            token.cancel();
            let error = create_range_parameter_with_control(
                "int4range",
                RangeParameterBounds::Empty,
                &Builtins,
                &Builtins,
                &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
            token.reset();
        }
    }
}

#[test]
fn range_and_multirange_descriptors_select_exact_native_types() {
    use crate::ast::RangeSubtype;
    let control = ProductionControl::uncontrolled();
    for subtype in [
        RangeSubtype::Integer,
        RangeSubtype::BigInteger,
        RangeSubtype::Numeric,
        RangeSubtype::Date,
        RangeSubtype::Timestamp,
        RangeSubtype::TimestampTz,
    ] {
        for ty in [ColumnType::Range(subtype), ColumnType::Multirange(subtype)] {
            let descriptor = RangeValueDescriptor::from_type_with_control(&ty, None, &control)
                .unwrap()
                .unwrap();
            assert_eq!(descriptor.endpoint, subtype.scalar_type());
            assert_eq!(
                *descriptor.range_type_with_control(None, &control).unwrap(),
                ColumnType::Range(subtype)
            );
        }
    }
    assert!(
        RangeValueDescriptor::from_type_with_control(&ColumnType::Text, None, &control)
            .unwrap()
            .is_none()
    );
}
