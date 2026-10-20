//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn image(id: i64, side: &str) -> Value {
    Value::Record(
        vec![
            ("id".into(), Value::Int(id)),
            (
                "body".into(),
                Value::Str(format!("{side}{id:04}{}", "x".repeat(32763))),
            ),
        ]
        .into(),
    )
}

#[test]
fn transition_capture_bounds_both_image_owners_and_preserves_order() {
    let allocated = allocation_counter::measure(|| {
        let token = CancellationToken::new();
        let mut old = TransitionImages::default();
        let mut new = TransitionImages::default();
        for id in 0..512 {
            old.push(&image(id, "o"), || Ok(16 * 1024), &token).unwrap();
            new.push(&image(id, "n"), || Ok(16 * 1024), &token).unwrap();
        }
        for (images, side) in [(old, "o"), (new, "n")] {
            let mut count = 0;
            for row in images.into_rows(token.clone()).unwrap() {
                assert!(row.unwrap().has_same_representation(&image(count, side)));
                count += 1;
            }
            assert_eq!(count, 512);
        }
    });
    // Capture and replay 32MiB of distinct OLD/NEW images under separate 16KiB allowances.
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn transition_capture_preserves_raw_identity_and_defers_field_observation() {
    let datum = uqa_core::DatumValue::new(1700, 0, vec![2, 0, 0, 0, 3, b'x']);
    let malformed = Value::Datum(datum.field(25, u32::MAX));
    let values = [
        Value::Record(uqa_core::RecordValue::from_parts(Vec::new(), Some(20001))),
        Value::Record(uqa_core::RecordValue::from_parts(
            vec![
                ("field".into(), Value::Int(7)),
                ("field".into(), Value::Int(9)),
                ("unread".into(), malformed.clone()),
                ("null".into(), Value::Null),
            ],
            Some(20002),
        )),
        Value::Null,
        Value::Int(3),
    ];
    for work_mem in [1, 64 * 1024] {
        let token = CancellationToken::new();
        let mut images = TransitionImages::default();
        for value in &values {
            images.push(value, || Ok(work_mem), &token).unwrap();
        }
        let mut rows = images.into_rows(token).unwrap();
        for expected in &values {
            let actual = rows.next().unwrap();
            assert!(actual.unwrap().has_same_representation(expected));
        }
        assert!(rows.next().is_none());
    }
    // Capture must not move this SQL observation error ahead of the consuming expression.
    assert!(uqa_sql::expr::value_to_string(&malformed).is_err());
}

#[test]
fn transition_capture_initializes_once_and_propagates_cancellation() {
    let token = CancellationToken::new();
    let mut images = TransitionImages::default();
    assert!(images.pending.is_none());
    let calls = std::cell::Cell::new(0);
    for value in [Value::Int(1), Value::Int(2)] {
        images
            .push(
                &value,
                || {
                    calls.set(calls.get() + 1);
                    Ok(64)
                },
                &token,
            )
            .unwrap();
    }
    assert_eq!(calls.get(), 1);
    let mut rows = images.into_rows(token.clone()).unwrap();
    assert_eq!(rows.next().unwrap().unwrap(), Value::Int(1));
    token.cancel();
    assert_eq!(rows.next().unwrap().unwrap_err().sqlstate(), Some("57014"));
    assert!(rows.next().is_none());
    let mut empty = TransitionImages::default();
    let error = empty
        .push(&Value::Int(1), || panic!("cancelled capture"), &token)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert!(empty.pending.is_none());
    assert_eq!(
        empty.into_rows(token).err().unwrap().sqlstate(),
        Some("57014")
    );
}
