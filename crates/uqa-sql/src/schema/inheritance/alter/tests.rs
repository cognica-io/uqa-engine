//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{validate_row_type, validate_row_type_with_work, ColumnDef, ReferenceWork};
use crate::{ColumnType, SQLError};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn arrays(depth: usize, mut ty: ColumnType) -> ColumnType {
    for _ in 0..depth {
        ty = ColumnType::Array(Box::new(ty));
    }
    ty
}

fn run(
    child_depth: usize,
    leaf: ColumnType,
    collation: Option<u32>,
    cancel_at: Option<usize>,
) -> (usize, Option<String>) {
    let parent = [ColumnDef::nullable("field", arrays(256, ColumnType::Text))];
    let mut child = [ColumnDef::nullable("field", arrays(child_depth, leaf))];
    child[0].collation_oid = collation;
    let describe = |e: &SQLError| (e.sqlstate().unwrap().to_owned(), e.to_string());
    let ordinary = validate_row_type(
        &parent,
        &child,
        "public.parent",
        "public.child",
        true,
        false,
    )
    .as_ref()
    .err()
    .map(describe);
    let memory = MemoryBudget::new(16 << 10);
    let token = CancellationToken::new();
    let original = token.clone();
    let mut decoder = ImageDecoder::new(&memory, &token, move |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(input.memory, &original, input.cancellation);
        validate_row_type_with_work(
            &parent,
            &child,
            "public.parent",
            "public.child",
            true,
            false,
            &mut ReferenceWork {
                control: &control,
                work: &mut input,
            },
        )
        .await
    })
    .unwrap();
    let mut calls = 0;
    let error = loop {
        if cancel_at == Some(calls) {
            token.cancel();
        }
        calls += 1;
        match decoder.advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 },
        ) {
            Ok(None) => (),
            Ok(Some(())) => break None,
            Err(error) => break Some(describe(&error)),
        }
    };
    drop(decoder);
    assert_eq!(memory.used(), 0);
    if cancel_at.is_none() {
        assert_eq!(error, ordinary);
    }
    (calls, error.map(|(state, _)| state))
}

#[test]
fn inherited_column_type_and_collation_yield_within_one_array_pair() {
    let first = run(0, ColumnType::Boolean, Some(950), None);
    let last = run(256, ColumnType::Boolean, Some(950), None);
    let collation = run(256, ColumnType::Text, Some(950), None);
    let equal = run(256, ColumnType::Text, None, None);
    assert_eq!(first.1.as_deref(), Some("42804"));
    assert_eq!(last.1, first.1);
    assert_eq!(collation.1.as_deref(), Some("42P21"));
    assert_eq!(equal.1, None);
    assert!(last.0 >= first.0 + 256);
    assert!(collation.0 >= last.0 + 256);
    assert!(equal.0 >= collation.0 + 256);
    let interrupted = run(256, ColumnType::Text, None, Some(first.0 + 128));
    assert_eq!(interrupted.1.as_deref(), Some("57014"));
}
