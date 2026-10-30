//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ast::DeferredSQLError, ColumnType, SQLParam};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn retained_parameter_copies_preserve_carriers_types_and_deferred_diagnostics() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(1 << 20);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let parameters = [
        SQLParam::Scalar(Value::Str("scalar".into())),
        SQLParam::TypedScalar {
            value: Value::Str("typed".into()),
            ty: ColumnType::Text,
        },
        SQLParam::DeferredError {
            ty: ColumnType::Integer,
            error: Box::new(DeferredSQLError {
                sqlstate: "XX001".into(),
                message: "unobserved field".into(),
                detail: Some("retained descriptor".into()),
                hint: Some("original diagnostic".into()),
            }),
        },
        SQLParam::Vector(vec![1.0, 2.0, 3.0]),
        SQLParam::Tensor(vec![vec![1.0, 2.0], vec![3.0, 4.0]]),
        SQLParam::tensor_with_control(&[], 3, &ProductionControl::uncontrolled())
            .unwrap()
            .into_uncontrolled()
            .unwrap(),
        SQLParam::tensor_with_control(&[1.0, 2.0, 3.0, 4.0], 2, &ProductionControl::uncontrolled())
            .unwrap()
            .into_uncontrolled()
            .unwrap(),
    ];
    for parameter in &parameters {
        let mut copy = None;
        let allocation = allocation_counter::measure(|| {
            copy = Some(parameter.clone_with_control(&control).unwrap());
        });
        let copy = copy.unwrap();
        assert_eq!(
            copy.reserved_bytes(),
            usize::try_from(allocation.bytes_current).unwrap()
        );
        assert_eq!(budget.used(), copy.reserved_bytes());
        assert_eq!(
            copy.declared_scalar_type(),
            parameter.declared_scalar_type()
        );
        match (parameter, &*copy) {
            (
                SQLParam::DeferredError { error, .. },
                SQLParam::DeferredError { error: copied, .. },
            ) => {
                assert_eq!(copied, error);
                let observed = copy.to_value().unwrap_err();
                assert_eq!(observed.sqlstate(), Some("XX001"));
                assert_eq!(observed.to_string(), "unobserved field");
                assert_eq!(observed.detail(), Some("retained descriptor"));
                assert_eq!(observed.hint(), Some("original diagnostic"));
            }
            (SQLParam::TypedTensor(tensor), SQLParam::TypedTensor(copied)) => {
                assert_eq!(copied.dimensions(), tensor.dimensions());
                assert_eq!(copied.row_count(), tensor.row_count());
                assert_eq!(copied.values(), tensor.values());
            }
            _ => assert_eq!(copy.to_value().unwrap(), parameter.to_value().unwrap()),
        }
        drop(copy);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn retained_parameter_copy_admits_before_allocation_and_preserves_both_cancellations() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let budget = MemoryBudget::new(8 << 20);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let parameter = SQLParam::DeferredError {
        ty: ColumnType::Text,
        error: Box::new(DeferredSQLError {
            sqlstate: "XX001".into(),
            message: "x".repeat(4 << 20),
            detail: None,
            hint: None,
        }),
    };
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            parameter
                .clone_with_control(&control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            parameter
                .clone_with_control(&control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    let copied = parameter.clone_with_control(&control).unwrap();
    assert!(copied.reserved_bytes() >= 4 << 20);
    assert_eq!(copied.declared_scalar_type(), Some(&ColumnType::Text));
    drop(copied);
    assert_eq!(budget.used(), 0);
}
