//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn json_value_input_preserves_original_and_invoking_cancellation() {
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let decoder =
            JsonValueDecoder::new(&memory, &original).with_invoking_cancellation(&invoking);
        let prior = decoder.value(r#""completed value""#).unwrap();
        let retained = memory.used();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        for result in [
            decoder.value("null").map(drop),
            decoder.fields("{}").map(drop),
            decoder.visit_fields("{}", &mut |_, _| panic!("cancelled field visitor")),
        ] {
            assert!(matches!(result, Err(JsonReadError::Cancelled(_))));
            assert_eq!(memory.used(), retained);
        }
        assert_eq!(*prior, Value::Str("completed value".into()));
        drop(prior);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn json_value_visitors_cancel_before_the_next_original_field() {
    let large = "f".repeat(1 << 20);
    let text =
        format!(r#"{{"first":"retained","second":{{"$uqa_type":"bytes","hex":"{large}"}}}}"#);
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let decoder =
            JsonValueDecoder::new(&memory, &original).with_invoking_cancellation(&invoking);
        let mut prior = None;
        let result = decoder.visit_fields(&text, &mut |name, value| {
            assert_eq!(name, "first");
            assert!(prior.is_none());
            prior = Some(value);
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            Ok(())
        });
        assert!(matches!(result, Err(JsonReadError::Cancelled(_))));
        assert_eq!(memory.used(), prior.as_ref().unwrap().reserved_bytes());
        assert_eq!(**prior.as_ref().unwrap(), Value::Str("retained".into()));
        drop(prior);
        assert_eq!(memory.used(), 0);
    }
}
