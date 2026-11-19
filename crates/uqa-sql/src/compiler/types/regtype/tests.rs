//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

mod reference;
type Decoder = ImageDecoder<Option<Produced<ParsedRegtypeName>>, SQLError>;

fn decoder(
    source: Arc<str>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        parse_regtype_name_from_input(&source, &control, &mut input).await
    })
    .unwrap()
}
fn advance(input: &mut Decoder, steps: usize) -> Result<Option<DecoderOutput>, SQLError> {
    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
}
type DecoderOutput = Option<Produced<ParsedRegtypeName>>;
fn complete(input: &mut Decoder, steps: usize) -> Result<(DecoderOutput, usize), SQLError> {
    for polls in 1..100_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, polls));
        }
    }
    panic!("type-name input stopped progressing")
}

#[test]
fn retained_regtype_projection_matches_original_parser_scanner_and_errors() {
    let memory = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for name in [
        "",
        " \t\n\r\u{b}\u{c}",
        "\u{2000}",
        "integer",
        "INTEGER",
        "int4",
        "\"integer\"",
        "\"a.b\".\"quoted\"\"name\"",
        "public.f",
        "pg_temp.f",
        "pg_catalog.int4",
        "\"pg_catalog\".\"char\"",
        "numeric(3,-2)",
        "timestamp(6) with time zone",
        "interval day to second(2)",
        "vector(3)",
        "domain#42",
        "a.b.c",
        "SETOF integer",
        "int4; SELECT 1",
        "int4 + 1",
        "int4\0",
        "\"unterminated",
        "-- comment\nint4",
        "/* array [] */ int4",
    ] {
        for suffix in ["", "[]", "[3][4]", " ARRAY", " ARRAY[7]", "(", " NOT NULL"] {
            let source: Arc<str> = format!("{name}{suffix}").into();
            let expected = reference::parse(&source, &ProductionControl::uncontrolled())
                .map(|value| value.map(|value| value.into_uncontrolled().unwrap()))
                .map_err(|error| (error.sqlstate().map(str::to_owned), error.to_string()));
            for steps in [1, 7, 128] {
                let mut input = decoder(Arc::clone(&source), &memory, &work, &token, &token);
                let actual = complete(&mut input, steps)
                    .map(|(value, _)| value.as_deref().cloned())
                    .map_err(|error| (error.sqlstate().map(str::to_owned), error.to_string()));
                assert_eq!(actual, expected, "{source:?}, {steps}");
                assert!(advance(&mut input, 1).is_err());
                assert_eq!((memory.used(), work.used()), (0, 0));
            }
        }
    }
}

#[test]
fn retained_regtype_projection_bounds_owned_work_and_retires_every_pending_state() {
    let source: Arc<str> =
        format!("{}\"가나다\".\"타입\"{}", " ".repeat(16385), "[]".repeat(8)).into();
    let memory = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (value, polls) = complete(
        &mut decoder(Arc::clone(&source), &memory, &work, &token, &token),
        1,
    )
    .unwrap();
    let value = value.unwrap();
    assert_eq!(value.names, ["가나다", "타입"]);
    assert_eq!(value.array_dimensions, 8);
    assert!(polls >= 25);
    assert_eq!(memory.used(), value.reserved_bytes());
    drop(value);
    let mut denials = 0;
    for pending in 0..polls - 1 {
        for action in ["original", "invoking", "drop", "memory", "window"] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(Arc::clone(&source), &memory, &work, &original, &invoking);
            for _ in 0..pending {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            let hold = (action == "memory").then(|| memory.reserve(memory.available()).unwrap());
            match action {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                _ => {}
            }
            if action != "drop" {
                let result = complete(&mut input, usize::from(action != "window"));
                if action == "memory" {
                    if let Err(error) = &result {
                        assert_eq!(error.sqlstate(), Some("53200"));
                        denials += 1;
                    }
                } else {
                    assert!(result.is_err());
                }
                drop(result);
            }
            drop((input, hold));
            assert_eq!((memory.used(), work.used()), (0, 0));
        }
    }
    assert!(denials > 0);
}
