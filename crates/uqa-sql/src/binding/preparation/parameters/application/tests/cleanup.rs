//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn command_source_observations_retire_wide_type_metadata_between_work_windows() {
    let native = MemoryBudget::new(1 << 24);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    control.check().unwrap();
    let source = functions(0);
    let Fixture::Source(source_plan) = source.as_ref() else {
        unreachable!()
    };
    let mut constants = observe(&source, &control);
    let ty = (0..32).fold(crate::ColumnType::Text, |child, _| {
        crate::ColumnType::Array(Box::new(child))
    });
    let schema = crate::RowSchema::with_column_metadata_with_control(
        None,
        std::iter::repeat_n(("value", Some(&ty), None), 64),
        &control,
    )
    .unwrap();
    constants
        .8
        .sources
        .insert(AnalysisIdentity::from(source_plan.as_ref()), schema)
        .unwrap();
    let exhausted = native.reserve(native.available()).unwrap();
    let original = token.clone();
    let native_input = native.clone();
    let mut input = ImageDecoder::new(&work, &token, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&native_input, &original, work.cancellation);
        let Fixture::Source(source_plan) = source.as_ref() else {
            unreachable!()
        };
        let bindings = constants
            .source_metadata_from(source_plan, &control, &mut work)
            .await?;
        assert!(constants.8.sources.is_empty());
        Ok::<_, SQLError>(bindings)
    })
    .unwrap();
    let mut polls = 0;
    loop {
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(
                input
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 },
                    )
                    .unwrap(),
            );
        });
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert!(allocation.count_current >= -96, "{allocation:?}");
        polls += 1;
        if let Some(bindings) = output.unwrap() {
            assert!(bindings.is_empty());
            break;
        }
        assert!(polls < 30_000);
    }
    assert!(polls > 64 * 32);
    drop(input);
    assert_eq!((native.used(), work.used()), (exhausted.bytes(), 0));
}
