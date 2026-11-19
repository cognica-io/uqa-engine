//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::RetainedValue,
};

type Source = Arc<Vec<(RelationIdentity, CatalogTableSnapshot)>>;
type Input = ImageDecoder<CatalogReadView, SQLError>;

fn source() -> Source {
    Arc::new(
        (0..3)
            .map(|index| {
                let table = crate::catalog::test_support::table_snapshot(
                    [index + 1; 16],
                    Vec::new(),
                    uqa_sql::ast::TableConstraintSet::default(),
                );
                (
                    RelationIdentity::new("schema_型".repeat(512), format!("table_{index}")),
                    table,
                )
            })
            .collect(),
    )
}

fn selected_table(
    (key, table): &(RelationIdentity, CatalogTableSnapshot),
) -> (&RelationIdentity, CatalogTableSnapshot) {
    (key, table.clone())
}

fn capture(catalog: &CatalogReadView) -> CatalogReadCapture {
    CatalogReadCapture::new(catalog.snapshot().definitions.clone()).with_temporary_namespace(
        u64::MAX,
        Some(TemporaryNamespaceOids {
            namespace: 50,
            toast_namespace: 51,
        }),
    )
}

fn input(
    capture: CatalogReadCapture,
    source: Source,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        capture
            .capture_from_input(
                source.iter().map(selected_table),
                &native,
                &original,
                &mut reader.input(),
            )
            .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<CatalogReadView>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (CatalogReadView, usize) {
    for polls in 1..100_000 {
        if let Some(view) = advance(input).unwrap() {
            return (view, polls);
        }
    }
    panic!("catalog read capture stopped progressing")
}

#[test]
fn retained_catalog_read_capture_preserves_all_selected_roots_and_temporary_identity() {
    let source = source();
    let catalog = crate::catalog::test_support::empty_catalog();
    let retained_tables = Arc::downgrade(&catalog.snapshot().definitions.foreign_tables);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let ordinary = capture(&catalog)
        .capture_with_control(source.iter().map(selected_table), &control)
        .unwrap();
    let ordinary_bytes = native.used();
    let (retained, polls) = complete(&mut input(
        capture(&catalog),
        source.clone(),
        &native,
        &work,
        &token,
        &token,
    ));
    assert!(polls > 10);
    assert_eq!(work.used(), 0);
    assert_eq!(native.used(), ordinary_bytes * 2);
    assert_eq!(
        retained.snapshot().temporary_namespace,
        ordinary.snapshot().temporary_namespace
    );
    assert_eq!(
        retained
            .snapshot()
            .temporary_namespace
            .as_ref()
            .unwrap()
            .schema,
        "pg_temp_18446744073709551615"
    );
    assert_eq!(retained.snapshot().tables.len(), 3);
    for (key, table) in &retained.snapshot().tables {
        let expected = &ordinary.snapshot().tables[key];
        assert_eq!(table.object_id, expected.object_id);
        assert!(table.columns.shares_definition(&expected.columns));
    }
    assert!(RetainedValue::ptr_eq(
        &retained.snapshot().definitions.roles,
        &catalog.snapshot().definitions.roles
    ));
    assert!(RetainedValue::ptr_eq(
        &retained.snapshot().definitions.database_security,
        &catalog.snapshot().definitions.database_security
    ));
    drop((ordinary, catalog, source));
    assert!(retained_tables.upgrade().is_some());
    drop(retained);
    assert!(retained_tables.upgrade().is_none());
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_catalog_read_capture_retires_all_prefixes_and_both_cancellation_owners() {
    let source = source();
    let catalog = crate::catalog::test_support::empty_catalog();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (view, polls) = complete(&mut input(
        capture(&catalog),
        source.clone(),
        &native,
        &work,
        &token,
        &token,
    ));
    drop(view);
    let sibling = native.reserve(19).unwrap();
    for prefix in 0..polls - 1 {
        for cancelled in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(
                capture(&catalog),
                source.clone(),
                &native,
                &work,
                &original,
                &invoking,
            );
            for _ in 0..prefix {
                assert!(advance(&mut input).unwrap().is_none());
            }
            if cancelled != 0 {
                if cancelled == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut input).err().unwrap().sqlstate(), Some("57014"));
                assert_eq!(advance(&mut input).err().unwrap().sqlstate(), Some("XX001"));
            }
            drop(input);
            assert_eq!((native.used(), work.used()), (19, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_catalog_read_capture_admits_empty_view_headers_and_retires_failed_publication() {
    let catalog = crate::catalog::test_support::empty_catalog();
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = Arc::new(Vec::new());
    for limit in [0, 1, size_of::<CatalogReadSnapshot>() / 2] {
        let native = MemoryBudget::new(limit);
        let input = input(
            capture(&catalog),
            source.clone(),
            &native,
            &work,
            &token,
            &token,
        );
        assert_eq!(
            input.read(&mut std::io::empty()).err().unwrap().sqlstate(),
            Some("53200")
        );
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    let native = MemoryBudget::new(1 << 20);
    let (view, _) = complete(&mut input(
        CatalogReadCapture::new(catalog.snapshot().definitions.clone()),
        source,
        &native,
        &work,
        &token,
        &token,
    ));
    assert!(view.snapshot().tables.is_empty());
    assert!(view.snapshot().temporary_namespace.is_none());
    assert!(native.used() >= size_of::<CatalogReadSnapshot>());
    drop(view);
    assert_eq!((native.used(), work.used()), (0, 0));
}
