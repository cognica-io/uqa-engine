//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, DatumValue, RecordValue,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("comparison policy must not interpret source bytes")
    }
}

#[test]
fn retained_comparison_policy_preserves_opaque_boundaries_and_short_circuiting() {
    let datum = Value::Datum(DatumValue::new(25, u32::MAX, vec![0xff]));
    let rows = [
        (Value::List(vec![Value::Int(7); 1024]), false),
        (Value::List(vec![Value::Int(7), datum.clone()]), true),
        (Value::Map([("opaque".into(), datum.clone())].into()), false),
        (
            Value::Record(RecordValue::from_parts(
                vec![("lazy".into(), datum.clone())],
                None,
            )),
            true,
        ),
        (
            Value::Record(RecordValue::from_parts(vec![], Some(42))),
            true,
        ),
        (
            Value::Array(
                uqa_core::ArrayValue::try_new(vec![Value::Int(7)])
                    .unwrap()
                    .with_element_type_oid(Some(23)),
            ),
            true,
        ),
        (Value::Row(vec![Value::Int(7)].into()), false),
    ];
    for (value, expected) in rows {
        assert_eq!(value_comparison_can_fail(&value), expected);
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input =
            ImageDecoder::<_, SQLError>::new(&budget, &token, |mut reader| async move {
                value_comparison_can_fail_from(&value, &mut reader.input()).await
            })
            .unwrap();
        let mut steps = 0;
        let result = loop {
            steps += 1;
            if let Some(result) = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                break result;
            }
        };
        assert_eq!(result, expected);
        if !expected {
            assert!(steps > 1);
        }
        drop(input);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn retained_comparison_policy_retires_pending_traversal_on_cancellation_and_denial() {
    for failure in ["cancel", "memory", "window", "drop"] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut value = Value::Int(7);
        for _ in 0..128 {
            value = Value::List(vec![value]);
        }
        let mut input =
            ImageDecoder::<_, SQLError>::new(&budget, &token, |mut reader| async move {
                value_comparison_can_fail_from(&value, &mut reader.input()).await
            })
            .unwrap();
        assert!(input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
            .is_none());
        let held = (failure == "memory").then(|| budget.reserve(budget.available()).unwrap());
        if failure == "cancel" {
            token.cancel();
        }
        if failure != "drop" {
            assert!(input
                .advance(
                    &mut NoRead,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: if failure == "window" { 0 } else { 16 }
                    }
                )
                .is_err());
        }
        drop((input, held));
        assert_eq!(budget.used(), 0, "{failure}");
    }
}
