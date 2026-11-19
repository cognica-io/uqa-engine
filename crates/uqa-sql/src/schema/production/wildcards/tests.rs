//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::PhysicalLayout;
use std::collections::HashSet;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn schema() -> RowSchema {
    RowSchema::from_physical_layout(PhysicalLayout {
        columns: vec!["later".into(), "first".into(), "absent".into()],
        identities: ["later", "first", "absent"]
            .map(|column| ColumnIdentity::qualified("r", column))
            .into(),
        types: vec![Some(ColumnType::Integer); 3],
        slots: vec![Some(2), Some(0), None],
        physical_width: 3,
        aliases: vec![
            (
                ColumnIdentity::qualified("r", "first"),
                Some(0),
                Some(ColumnType::Integer),
            ),
            (
                ColumnIdentity::qualified("r", "hidden"),
                Some(1),
                Some(ColumnType::Text),
            ),
        ],
        internal: vec![],
        score_sources: vec![],
        wildcard_hidden: HashSet::new(),
    })
    .unwrap()
}

#[test]
fn borrowed_wildcard_positions_match_visible_hidden_and_null_layout() {
    let schema = schema();
    let expected = schema.qualified_star_position_layout("r");
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut actual = Vec::new();
    schema
        .visit_qualified_star_positions("r", &control, |name, logical, slot, ty| {
            actual.push((name.to_string(), logical, slot, ty.cloned()));
            Ok(())
        })
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn wildcard_failure_and_cancellation_release_sort_scratch() {
    let schema = schema();
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(29).unwrap();
    let mut visited = 0;
    let error = schema
        .visit_qualified_star_positions("r", &control, |_, _, _, _| {
            visited += 1;
            invoking.cancel();
            Ok(())
        })
        .unwrap_err();
    assert_eq!(visited, 1);
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(budget.used(), 29);
    invoking.reset();
    let error = schema
        .visit_qualified_star_positions("r", &control, |_, _, _, _| {
            Err(SQLError::Internal("consumer failed".into()))
        })
        .unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "consumer failed"));
    assert_eq!(budget.used(), 29);
    let empty = budget.child(0);
    let empty_control = ProductionControl::new(&empty, &original, &invoking);
    assert_eq!(
        schema
            .visit_qualified_star_positions("r", &empty_control, |_, _, _, _| panic!(
                "unadmitted visitor"
            ))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

struct StarResult {
    calls: usize,
    first_field: Option<usize>,
    most_in_one_call: usize,
    fields: Result<Vec<String>, SQLError>,
}

fn selected_star(
    row: RowSchema,
    expression: crate::ScalarExpr,
    steps: usize,
    cancel: Option<bool>,
    allowance: usize,
) -> StarResult {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
    let native = MemoryBudget::new(allowance);
    let retained = native.clone();
    let work = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let captured = original.clone();
    let invoking = CancellationToken::new();
    let emitted = Arc::new(AtomicUsize::new(0));
    let selected = emitted.clone();
    let mut decoder = ImageDecoder::new(&work, &invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&retained, &captured, channel.cancellation);
        let mut names = Vec::new();
        let star = crate::semantics::visit_bound_projection_star_metadata_from(
            &expression,
            &row,
            &row,
            &control,
            &mut channel,
            &mut |field| {
                selected.fetch_add(1, Ordering::Relaxed);
                names.push(field.name.to_owned());
                Ok(())
            },
        )
        .await?;
        assert!(star);
        Ok::<_, SQLError>(names)
    })
    .unwrap();
    let mut calls = 0;
    let mut first_field = None;
    let mut most_in_one_call = 0;
    let fields = loop {
        calls += 1;
        let before = emitted.load(Ordering::Relaxed);
        if before > 0 {
            match cancel {
                Some(true) => original.cancel(),
                Some(false) => invoking.cancel(),
                None => (),
            }
        }
        let state = decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps });
        let count = emitted.load(Ordering::Relaxed) - before;
        if count > 0 {
            first_field.get_or_insert(calls);
        }
        most_in_one_call = most_in_one_call.max(count);
        match state {
            Ok(Some(names)) => break Ok(names),
            Ok(None) => (),
            Err(error) => break Err(error),
        }
    };
    drop(decoder);
    assert_eq!((native.used(), work.used()), (0, 0));
    StarResult {
        calls,
        first_field,
        most_in_one_call,
        fields,
    }
}

#[test]
fn wildcard_metadata_schedules_each_visible_field_and_retires_qualified_sort_scratch() {
    use crate::ScalarExpr;
    let qualified = || ScalarExpr::QualifiedStar("r".into());
    for count in [2, 32] {
        let names = (0..count).map(|i| format!("field_{i}")).collect::<Vec<_>>();
        let ty = ColumnType::Integer;
        let fields = names.iter().map(|name| (name.as_str(), Some(&ty), None));
        let row = RowSchema::with_column_metadata_with_control(
            Some("r"),
            fields,
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        let bare = selected_star(row.clone(), ScalarExpr::Star, 1, None, 1 << 20);
        let allowance = if count == 2 { 16 << 10 } else { 1 << 20 };
        let qualified_step = selected_star(row.clone(), qualified(), 1, None, allowance);
        let batch = selected_star(row.clone(), qualified(), 32, None, 1 << 20);
        assert_eq!(bare.fields.unwrap(), names);
        assert_eq!(qualified_step.fields.unwrap(), names);
        assert_eq!(batch.fields.unwrap(), names);
        assert_eq!(bare.most_in_one_call, 1);
        assert_eq!(qualified_step.most_in_one_call, 1);
        assert!(batch.most_in_one_call <= 32 && batch.calls < qualified_step.calls);
        assert!(qualified_step.first_field.unwrap() >= bare.first_field.unwrap() + count);
        for original in [true, false] {
            let result = selected_star(row.clone(), qualified(), 1, Some(original), 1 << 20);
            assert_eq!(result.fields.unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(result.most_in_one_call, 1);
        }
        let denied = selected_star(row, qualified(), 1, None, 1);
        assert_eq!(denied.fields.unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(denied.first_field, None);
    }
    let input = schema();
    let result = selected_star(input, qualified(), 1, None, 1 << 20);
    assert_eq!(
        result.fields.unwrap(),
        ["first", "hidden", "later", "absent"]
    );
}
