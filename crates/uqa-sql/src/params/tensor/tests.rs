//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn packed_tensors_preserve_empty_shape_and_float_bits_through_copy_and_scalar_conversion() {
    let control = ProductionControl::uncontrolled();
    for values in [
        vec![],
        vec![-0.0, f32::MIN_POSITIVE, f32::MAX, 1.5, -2.0, 3.0],
    ] {
        let parameter = SQLParam::tensor_with_control(&values, 3, &control).unwrap();
        let SQLParam::TypedTensor(tensor) = &*parameter else {
            panic!("typed tensor");
        };
        assert_eq!(tensor.dimensions(), 3);
        assert_eq!(tensor.row_count(), values.len() / 3);
        let copy = tensor.copy_with_control(&control).unwrap();
        let scalar = parameter.to_value_with_control(&control).unwrap();
        let observed = SQLTensor::from_value_with_control(&scalar, 3, &control).unwrap();
        for value in [&*copy, &*observed] {
            assert_eq!(value.dimensions(), 3);
            assert_eq!(
                value
                    .values()
                    .iter()
                    .map(|x| x.to_bits())
                    .collect::<Vec<_>>(),
                values.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn tensor_input_and_observation_release_failed_admission_and_honor_both_cancellations() {
    let budget = MemoryBudget::new(64);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = control.copy_text("held").unwrap();
    for (values, dimensions) in [
        (vec![], 0),
        (vec![1.0], 2),
        (vec![f32::NAN], 1),
        (vec![f32::INFINITY], 1),
    ] {
        assert!(SQLParam::tensor_with_control(&values, dimensions, &control).is_err());
        assert_eq!(budget.used(), held.reserved_bytes());
    }
    assert_eq!(
        SQLParam::tensor_with_control(&[1.0; 64], 2, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    let malformed = Value::List(vec![
        Value::List(vec![Value::Float(1.0)]),
        Value::List(vec![]),
    ]);
    assert!(SQLTensor::from_value_with_control(&malformed, 1, &control).is_err());
    assert_eq!(budget.used(), held.reserved_bytes());
    let empty_tensor = SQLTensor::from_slice_with_control(&[], 3, &control).unwrap();
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            empty_tensor
                .copy_with_control(&control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        token.reset();

        token.cancel();
        assert_eq!(
            SQLParam::tensor_with_control(&[], 3, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            SQLTensor::from_value_with_control(&Value::List(vec![]), 3, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    let empty = SQLParam::tensor_with_control(&[], u32::MAX, &control).unwrap();
    assert_eq!(empty.reserved_bytes(), 0);
    drop(empty);
    drop(held);
    assert_eq!(budget.used(), 0);
}
