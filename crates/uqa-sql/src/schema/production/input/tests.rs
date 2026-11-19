//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

#[derive(Clone)]
struct Source {
    qualifier: String,
    relation: InternalRelationId,
    columns: Vec<(String, Option<ColumnType>)>,
}

type Input = ImageDecoder<RowSchema, SQLError>;

fn input(
    source: Arc<Source>,
    native: &MemoryBudget,
    transport: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(transport, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut columns = ProductionVec::new(control);
        columns.reserve(source.columns.len())?;
        for (name, ty) in &source.columns {
            input.checkpoint().await?;
            columns.push_copy((name.as_str(), ty.as_ref()))?;
        }
        RowSchema::with_parameter_metadata_from_input(
            &source.qualifier,
            source.relation,
            &columns,
            &native,
            &original,
            &mut input,
        )
        .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<RowSchema>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (RowSchema, usize) {
    for polls in 1..200_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("parameter layout stopped progressing")
}

fn source(wide: bool) -> Arc<Source> {
    let name = if wide {
        "argument_型.é🦀".repeat(900)
    } else {
        "value".into()
    };
    Arc::new(Source {
        qualifier: if wide {
            "routine_型\"".repeat(900)
        } else {
            "routine.dot".into()
        },
        relation: InternalRelationId::allocate(),
        columns: vec![
            (name.clone(), Some(ColumnType::Text)),
            (name, Some(ColumnType::Integer)),
            ("".into(), None),
            (
                "quoted.column".into(),
                Some(ColumnType::Domain {
                    schema: "public".into(),
                    name: "selected".into(),
                    oid: 70001,
                    array_oid: Some(70002),
                    collation_oid: Some(950),
                    base: Box::new(ColumnType::Array(Box::new(ColumnType::Varchar(Some(71))))),
                }),
            ),
        ],
    })
}

fn ordinary(source: &Source) -> RowSchema {
    let visible = RowSchema::with_qualified_types(
        &source.qualifier,
        source
            .columns
            .iter()
            .map(|(name, _)| name.clone())
            .collect(),
        source.columns.iter().map(|(_, ty)| ty.clone()).collect(),
    );
    let marked = RowSchema::with_internal_relation_types(
        source.relation,
        source.columns.iter().map(|(_, ty)| ty.clone()).collect(),
    );
    RowSchema::with_trailing_internal_aliases(&visible, &marked)
}

#[test]
fn retained_parameter_layout_preserves_native_names_duplicates_types_and_empty_namespaces() {
    let native = MemoryBudget::new(8 << 20);
    let transport = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for source in [
        source(false),
        source(true),
        Arc::new(Source {
            qualifier: "empty.型".repeat(700),
            relation: InternalRelationId::allocate(),
            columns: vec![],
        }),
    ] {
        let expected = ordinary(&source);
        let (schema, polls) = complete(&mut input(
            source.clone(),
            &native,
            &transport,
            &token,
            &token,
        ));
        assert!(polls > 2);
        assert_eq!(schema, expected);
        assert!(schema.has_qualifier(&source.qualifier));
        for (position, (name, ty)) in source.columns.iter().enumerate() {
            assert_eq!(schema.column_slot(name), expected.column_slot(name));
            assert_eq!(
                schema.qualified_slot(&source.qualifier, name),
                expected.qualified_slot(&source.qualifier, name)
            );
            assert_eq!(
                schema.internal_slot(source.relation.column(position)),
                Some(position)
            );
            assert_eq!(
                serde_json::to_value(schema.column_type(position)).unwrap(),
                serde_json::to_value(ty).unwrap()
            );
        }
        assert_eq!(transport.used(), 0);
        let held = native.used();
        let clone = schema.clone();
        drop(schema);
        assert_eq!(native.used(), held);
        assert_eq!(clone, expected);
        drop(clone);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_parameter_layout_retires_every_pending_index_and_preserves_sibling_allowances() {
    let source = source(false);
    let native = MemoryBudget::new(1 << 20);
    let transport = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (schema, polls) = complete(&mut input(
        source.clone(),
        &native,
        &transport,
        &token,
        &token,
    ));
    drop(schema);
    let sibling = native.reserve(31).unwrap();
    for prefix in 0..polls - 1 {
        for outcome in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &transport, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if outcome != 0 {
                if outcome == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("XX001"));
            }
            drop(reader);
            assert_eq!((native.used(), transport.used()), (31, 0));
        }
    }
    drop(sibling);
}

#[test]
fn retained_parameter_layout_admits_all_native_buffers_and_indexes_before_publication() {
    let source = source(true);
    let transport = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let native = MemoryBudget::new(8 << 20);
    let (schema, _) = complete(&mut input(
        source.clone(),
        &native,
        &transport,
        &token,
        &token,
    ));
    drop(schema);
    for available in [0, 1, 256, 8192, 65536, native.peak() - 1] {
        let native = MemoryBudget::new(available);
        let reader = input(source.clone(), &native, &transport, &token, &token);
        let error = reader.read(&mut std::io::empty()).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((native.used(), transport.used()), (0, 0));
        assert!(native.peak() <= available);
    }
}

#[test]
fn retained_parameter_layout_accounts_for_wide_comparisons_and_every_parameter_visit() {
    let transport = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut observations = Vec::new();
    for (count, prefix) in [
        (16, "p".to_owned()),
        (160, "p".to_owned()),
        (16, "型".repeat(8192)),
    ] {
        let source = Arc::new(Source {
            qualifier: "function".into(),
            relation: InternalRelationId::allocate(),
            columns: (0..count)
                .map(|i| (format!("{prefix}_{i}"), Some(ColumnType::Integer)))
                .collect(),
        });
        let native = MemoryBudget::new(16 << 20);
        let (schema, polls) = complete(&mut input(
            source.clone(),
            &native,
            &transport,
            &token,
            &token,
        ));
        assert_eq!(schema, ordinary(&source));
        assert!(polls > count);
        observations.push(polls);
        drop(schema);
        assert_eq!((native.used(), transport.used()), (0, 0));
    }
    assert!(observations[1] > observations[0] * 5);
    // Copying each additional name byte must consume native work even when its index membership is unchanged.
    assert!(observations[2] >= observations[0] + ("型".len() * 8192 / 4096) * 16);
}
