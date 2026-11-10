//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::{Budgeted, MemoryBudget},
    CancellationToken, RelationIdentity,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("canonical name rendering must not read source bytes")
    }
}

fn input(
    name: String,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ImageDecoder<Budgeted<String>> {
    ImageDecoder::new(memory, cancellation, |mut reader| async move {
        RelationIdentity::borrowed_reference(&name)
            .unwrap()
            .render_from(&mut reader.input())
            .await
            .map_err(|error| match error {
                ValueRetentionError::Memory(error) => ImageIoError::Memory(error),
                ValueRetentionError::Cancelled(error) => ImageIoError::Cancelled(error),
                error @ ValueRetentionError::Malformed { .. } => {
                    panic!("valid relation name failed: {error}")
                }
            })
    })
    .unwrap()
}

#[test]
fn retained_relation_rendering_resumes_quoted_components_with_exact_native_capacity() {
    let cancellation = CancellationToken::new();
    for (source, expected) in [
        ("public.table".into(), "public.table".into()),
        ("\"foo\".\"bar\"".into(), "foo.bar".into()),
        (
            "\"A.Schema\".\"a\"\"b\"".into(),
            "\"A.Schema\".\"a\"\"b\"".into(),
        ),
        ("\"한국어\"".into(), "\"한국어\"".into()),
        (
            format!("\"{}\".\"{}\"", "a\"\"한🙂".repeat(4096), "z".repeat(16384)),
            format!("\"{}\".{}", "a\"\"한🙂".repeat(4096), "z".repeat(16384)),
        ),
    ] {
        let ordinary_memory = MemoryBudget::new(2 << 20);
        let control = ProductionControl::new(&ordinary_memory, &cancellation, &cancellation);
        let ordinary = RelationIdentity::borrowed_reference(&source)
            .unwrap()
            .render_with_control(&control)
            .unwrap();
        let memory = MemoryBudget::new(2 << 20);
        let mut input = input(source, &memory, &cancellation);
        let mut calls = 0;
        let value = loop {
            calls += 1;
            if let Some(value) = input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
            {
                break value;
            }
        };
        assert_eq!(*value, expected);
        assert_eq!(*value, *ordinary);
        assert_eq!(value.capacity(), ordinary.capacity());
        assert_eq!(value.reserved_bytes(), ordinary_memory.used());
        assert!(calls >= expected.chars().count().div_ceil(1024));
        drop((value, input, ordinary));
        assert_eq!(memory.used(), 0);
        assert_eq!(ordinary_memory.used(), 0);
    }
}

#[test]
fn retained_relation_rendering_retires_partial_names_on_every_terminal_path() {
    for failure in ["cancel", "memory", "window", "drop"] {
        let memory = MemoryBudget::new(2 << 20);
        let cancellation = CancellationToken::new();
        let mut input = input(
            format!("\"{}\".item", "한\"\"🙂".repeat(8192)),
            &memory,
            &cancellation,
        );
        for _ in 0..36 {
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .unwrap()
                .is_none());
        }
        let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if failure == "cancel" {
            cancellation.cancel();
        }
        if failure != "drop" {
            let mut failed = false;
            for _ in 0..128 {
                match input.advance(
                    &mut NoRead,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: usize::from(failure != "window"),
                    },
                ) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("failed name rendering published: {failure}"),
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            assert!(failed, "{failure}");
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
                .is_err());
        }
        drop((input, full));
        assert_eq!(memory.used(), 0, "{failure}");
    }
}
