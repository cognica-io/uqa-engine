//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    plan::{retirement::empty_query, AnalyzedSource},
    ColumnType,
};

fn retained_layout(width: usize, depth: usize, control: &ProductionControl<'_>) -> RowSchema {
    let ty = (0..depth).fold(ColumnType::Text, |child, _| {
        ColumnType::Array(Box::new(child))
    });
    RowSchema::with_column_metadata_with_control(
        Some("old"),
        std::iter::repeat_n(("value", Some(&ty), Some(951)), width),
        control,
    )
    .unwrap()
}

fn query(width: usize, depth: usize, control: &ProductionControl<'_>) -> QueryPlan {
    let mut query = empty_query();
    let schema = retained_layout(width, depth, control);
    let leaf = || AnalyzedSource {
        schema: schema.clone(),
        children: None,
    };
    let source = AnalyzedSource {
        schema: schema.clone(),
        children: Some(Box::new((leaf(), leaf()))),
    };
    let source = control
        .finish(
            source,
            control.reserve(size_of::<AnalyzedSource>()).unwrap(),
        )
        .unwrap();
    query.analyzed_source = Some(SharedAnalyzedSource::from_produced(source, control).unwrap());
    query.analyzed_output = Some(schema);
    query
}

fn decoder(
    mut query: QueryPlan,
    update: QueryLayoutUpdate,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<QueryPlan, SQLError> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&native, &original, work.cancellation);
        update.install_from(&mut query, &control, &mut work).await?;
        Ok(query)
    })
    .unwrap()
}

fn advance(input: &mut ImageDecoder<QueryPlan, SQLError>) -> Result<Option<QueryPlan>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn query_layout_replacement_retires_old_generations_with_bounded_native_work() {
    let native = MemoryBudget::new(1 << 24);
    let work = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&native, &original, &invoking);
    control.check().unwrap();
    let query = query(64, 32, &control);
    let sibling = native.reserve(37).unwrap();
    let exhausted = native.reserve(native.available()).unwrap();
    let mut input = decoder(
        query,
        QueryLayoutUpdate::default(),
        &native,
        &work,
        &original,
        &invoking,
    );
    let mut polls = 0;
    loop {
        let mut result = None;
        let allocation = allocation_counter::measure(|| result = Some(advance(&mut input)));
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert!(allocation.count_current >= -96, "{allocation:?}");
        polls += 1;
        if let Some(query) = result.unwrap().unwrap() {
            assert!(query.analyzed_output.is_none());
            assert!(query.analyzed_source.is_none());
            assert_eq!(native.used(), sibling.bytes() + exhausted.bytes());
            break;
        }
        assert!(polls < 50_000);
    }
    assert!(polls > 64 * 32);
    drop((input, exhausted));
    assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
}

#[test]
fn query_layout_replacement_cleans_every_suspended_prefix_and_both_cancellation_scopes() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    let mut completed = false;
    for prefix in 0..2000 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let query = query(2, 2, &control);
            let update = QueryLayoutUpdate {
                output: Some(retained_layout(1, 1, &control)),
                source: None,
            };
            let expected = update.output.as_ref().unwrap().clone();
            let mut input = decoder(query, update, &native, &work, &original, &invoking);
            for _ in 0..prefix {
                if let Some(query) = advance(&mut input).unwrap() {
                    assert!(query
                        .analyzed_output
                        .as_ref()
                        .unwrap()
                        .shares_index(&expected));
                    assert!(query.analyzed_source.is_none());
                    completed = true;
                    break;
                }
            }
            if !completed && cancel != 0 {
                if cancel == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
            }
            let allocation = allocation_counter::measure(|| drop(input));
            assert_eq!(allocation.count_total, 0, "prefix {prefix}: {allocation:?}");
            assert_eq!(expected.qualified_slot("old", "value"), Some(0));
            drop(expected);
            assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
        }
        if completed {
            assert!(prefix > 20);
            break;
        }
    }
    assert!(completed);
}
