//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::TriggerEvent;

#[test]
fn after_event_spill_preserves_opaque_images_and_cancellation() {
    let datum = uqa_core::DatumValue::new(1700, 0, vec![2, 0, 0, 0, 3, b'x']);
    let values = [
        Value::Record(uqa_core::RecordValue::from_parts(
            vec![
                ("duplicate".into(), Value::Int(1)),
                ("duplicate".into(), Value::Int(2)),
                ("unread".into(), Value::Datum(datum.field(25, u32::MAX))),
                ("signed".into(), Value::Float(-0.0)),
                (
                    "nan".into(),
                    Value::Float(f64::from_bits(0x7ff8_0000_0000_0123)),
                ),
            ],
            Some(20001),
        )),
        Value::Record(uqa_core::RecordValue::from_parts(Vec::new(), Some(20002))),
        Value::Null,
        Value::Bytes(vec![0, 255, 10]),
    ];
    for bytes in [1, 64 * 1024] {
        let token = CancellationToken::new();
        let mut buffer = AfterRowEvents::default();
        for value in &values {
            buffer
                .push(
                    AfterRowTriggerEvent {
                        table: "public.source".into(),
                        event: TriggerEvent::Update,
                        old: value.clone(),
                        new: value.clone(),
                        captured: true,
                        triggers: Vec::new(),
                        foreign_keys: Vec::new(),
                    },
                    || Ok(bytes),
                    &token,
                )
                .unwrap();
        }
        for value in &values {
            let actual = buffer.take_next(&token).unwrap().unwrap();
            assert!(actual.old.has_same_representation(value));
            assert!(actual.new.has_same_representation(value));
        }
        assert!(buffer.take_next(&token).unwrap().is_none());
        token.cancel();
        assert_eq!(
            buffer.take_next(&token).err().unwrap().sqlstate(),
            Some("57014")
        );
        let mut empty = EventBuffer::<u64>::default();
        assert_eq!(
            empty
                .push(&1, || panic!("cancelled buffer"), &token)
                .unwrap_err()
                .sqlstate(),
            Some("57014"),
        );
        assert!(empty.rows.is_none());
    }
}

#[test]
fn after_event_buffer_rejects_corrupt_and_trailing_metadata_without_advancing() {
    for bytes in [vec![0xff], vec![0x01, 0x02]] {
        let token = CancellationToken::new();
        let mut buffer = EventBuffer::<u64>::default();
        buffer.push(&1, || Ok(1), &token).unwrap();
        buffer
            .rows
            .as_mut()
            .unwrap()
            .push(&PhysicalRow::from_values(vec![Value::Bytes(bytes)]))
            .unwrap();
        assert_eq!(buffer.take_next(&token).unwrap(), Some(1));
        for _ in 0..2 {
            assert!(buffer.take_next(&token).is_err());
            assert_eq!(buffer.position(), 1);
        }
    }
}
