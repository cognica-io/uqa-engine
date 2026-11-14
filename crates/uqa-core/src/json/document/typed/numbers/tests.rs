//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    json::{JsonDocument, JsonInputError, JsonReadError, JsonReadWork, JsonScalarProtocol},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn protocol() -> JsonScalarProtocol {
    JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap()
}

#[test]
fn generic_document_typed_integers_preserve_each_width_and_value_diagnostics() {
    let texts = [
        "null",
        "true",
        "[]",
        "{}",
        "\"한🙂\"",
        "-0",
        "0",
        "1.0",
        "1e0",
        "-1",
        "127",
        "128",
        "255",
        "256",
        "-128",
        "-129",
        "32767",
        "32768",
        "65535",
        "65536",
        "-32768",
        "-32769",
        "2147483647",
        "2147483648",
        "-2147483648",
        "-2147483649",
        "4294967295",
        "4294967296",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "-9223372036854775809",
        "18446744073709551615",
        "18446744073709551616",
        "1e999999",
    ];
    for text in texts {
        let generic: serde_json::Value = serde_json::from_str(text).unwrap();
        let memory = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&memory, &cancellation, &cancellation);
        let source =
            JsonDocument::read_with_control(text.as_bytes(), &control, protocol()).unwrap();
        macro_rules! compare {
            ($ty:ty, $read:ident) => {{
                let expected = serde_json::from_value::<$ty>(generic.clone());
                let actual = crate::json::work::inline(source.root().$read(&control, &mut ()));
                match (expected, actual) {
                    (Ok(expected), Ok(actual)) => {
                        assert_eq!(actual, expected, "{text}: {}", stringify!($ty))
                    }
                    (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
                        assert_eq!(
                            actual.to_string(),
                            expected.to_string(),
                            "{text}: {}",
                            stringify!($ty)
                        );
                        assert_eq!(actual.is_data(), expected.is_data());
                        assert_eq!(actual.is_syntax(), expected.is_syntax());
                        assert_eq!(actual.position().line, expected.line());
                        assert_eq!(actual.position().column, expected.column());
                    }
                    _ => panic!("typed {} differs for {text}", stringify!($ty)),
                }
            }};
        }
        compare!(u8, read_u8_with_work);
        compare!(u32, read_u32_with_work);
        compare!(std::num::NonZeroU32, read_nonzero_u32_with_work);
        compare!(u64, read_u64_with_work);
        compare!(usize, read_usize_with_work);
        compare!(i16, read_i16_with_work);
        compare!(i32, read_i32_with_work);
        compare!(i64, read_i64_with_work);
        drop(source);
        assert_eq!(memory.used(), 0);
    }
}

struct CancelAtCheckpoint(CancellationToken);
impl JsonReadWork for CancelAtCheckpoint {
    type Error = JsonReadError;
    async fn checkpoint(&mut self) -> Result<(), JsonReadError> {
        self.0.cancel();
        Ok(())
    }
}

#[test]
fn generic_document_projected_scalars_check_both_owners_after_suspended_work() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source_token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        b"42",
        &ProductionControl::new(&source_memory, &source_token, &source_token),
        protocol(),
    )
    .unwrap();
    let boolean = JsonDocument::read_with_control(
        b"true",
        &ProductionControl::new(&source_memory, &source_token, &source_token),
        protocol(),
    )
    .unwrap();
    for (original, number) in [(true, true), (false, true), (true, false), (false, false)] {
        let memory = MemoryBudget::new(0);
        let one = CancellationToken::new();
        let two = CancellationToken::new();
        let control = ProductionControl::new(&memory, &one, &two);
        let mut work = CancelAtCheckpoint(if original { one.clone() } else { two.clone() });
        let result = if number {
            crate::json::work::inline(source.root().read_u32_with_work(&control, &mut work))
                .map(|_| ())
        } else {
            crate::json::work::inline(boolean.root().read_bool_with_work(&control, &mut work))
                .map(|_| ())
        };
        assert!(matches!(result, Err(JsonInputError::Cancelled(_))));
        assert_eq!(memory.used(), 0);
    }
    drop(boolean);
    drop(source);
    assert_eq!(source_memory.used(), 0);
}

#[test]
fn generic_document_typed_integer_adopts_no_source_or_scalar_allocation() {
    let source_memory = MemoryBudget::new(1 << 20);
    let source_token = CancellationToken::new();
    let source = JsonDocument::read_with_control(
        b"18446744073709551615",
        &ProductionControl::new(&source_memory, &source_token, &source_token),
        protocol(),
    )
    .unwrap();
    let memory = MemoryBudget::new(0);
    let control = ProductionControl::new(&memory, &source_token, &source_token);
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(
            crate::json::work::inline(source.root().read_u64_with_work(&control, &mut ())).unwrap(),
        );
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(result, Some(u64::MAX));
    assert_eq!(memory.used(), 0);
    drop(source);
    assert_eq!(source_memory.used(), 0);
    assert_eq!(result, Some(u64::MAX));
}
