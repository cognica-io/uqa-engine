//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;
use uqa_core::{memory::BudgetedString, value_image::write_value_image, Value};
use uqa_storage::{
    diskann_index::{DiskANNIndexOptions, DiskANNMemoryIndex},
    inverted_index::AnalyzerBindings,
    vector_index::{DiskANNIndexParams, MemoryVectorIndex},
    DocumentStore, HNSWIndex, IVFIndex, InvertedIndex, ValueIndexKey, VectorIndex,
};

type Unit = (DecodedMemoryTableUnit, Vec<u8>);

fn name(value: &str) -> BudgetedString {
    let control = StorageReadControl::with_limit(1 << 20);
    let production = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    );
    BudgetedString::from_budgeted(
        production
            .copy_text(value)
            .unwrap()
            .into_budgeted()
            .unwrap(),
    )
}

fn vector(kind: usize) -> Box<dyn VectorIndex> {
    let mut value: Box<dyn VectorIndex> = match kind {
        0 => Box::new(MemoryVectorIndex::new(2)),
        1 => Box::new(IVFIndex::with_params(2, 2, 1, 4)),
        2 => Box::new(HNSWIndex::new(2)),
        3 => Box::new(
            DiskANNMemoryIndex::new(
                2,
                DiskANNIndexOptions::for_parameters(DiskANNIndexParams::for_dimensions(2).unwrap()),
                &DiskANNTemporaryBudget::new(8 << 20),
                &StorageReadControl::with_limit(32 << 20),
            )
            .unwrap(),
        ),
        _ => unreachable!(),
    };
    value.add(7, vec![1.0, 0.0]).unwrap();
    value.add(9, vec![0.0, 1.0]).unwrap();
    value.initialize().unwrap();
    value
}

fn fixture(kind: usize) -> Vec<Unit> {
    let output = StorageReadControl::with_limit(32 << 20);
    let mut document = MemoryDocumentStore::new();
    document
        .put(
            7,
            BTreeMap::from([("body".into(), Value::Str("original words".into()))]),
        )
        .unwrap();
    let mut document_bytes = vec![];
    document
        .write_image_document(7, &mut document_bytes, &output)
        .unwrap();
    let mut text = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    text.add_document(
        7,
        BTreeMap::from([("body".into(), "original words".into())]),
    )
    .unwrap();
    let mut text_bytes = vec![];
    text.write_image_document(7, &mut text_bytes, &output)
        .unwrap();
    let mut vector_bytes = vec![];
    vector(kind)
        .write_image(&mut vector_bytes, &output)
        .unwrap();
    // Independent evaluated-index metadata: ordered policy, no carried/temporal/row flags, two values and original field "body".
    let mut metadata = b"UQAEVI01\0\0\0\0".to_vec();
    metadata.extend_from_slice(&2_u64.to_le_bytes());
    metadata.extend_from_slice(&4_u64.to_le_bytes());
    metadata.extend_from_slice(b"body");
    let mut values = [vec![], vec![]];
    for (bytes, value) in values
        .iter_mut()
        .zip([Value::Null, Value::Str("stored expression".into())])
    {
        write_value_image(&value, bytes, output.memory(), output.cancellation()).unwrap();
    }
    vec![
        (DecodedMemoryTableUnit::Document(7), document_bytes),
        (DecodedMemoryTableUnit::Text(7), text_bytes),
        (
            DecodedMemoryTableUnit::Vector(name("embedding")),
            vector_bytes,
        ),
        (
            DecodedMemoryTableUnit::ValueIndex {
                name: name("opaque\0key"),
                document: None,
            },
            metadata,
        ),
        (
            DecodedMemoryTableUnit::ValueIndex {
                name: name("opaque\0key"),
                document: Some(7),
            },
            values[0].clone(),
        ),
        (
            DecodedMemoryTableUnit::ValueIndex {
                name: name("opaque\0key"),
                document: Some(9),
            },
            values[1].clone(),
        ),
    ]
}

fn builder(control: &StorageReadControl, registry: &MemoryCaptureRegistry) -> MemoryTableDataInput {
    MemoryTableDataInput::new(
        AnalyzerBindings::new(uqa_analysis::whitespace_analyzer()).default_binding(),
        std::iter::empty(),
        registry,
        control,
        1 << 20,
        &DiskANNTemporaryBudget::new(8 << 20),
    )
    .unwrap()
}

fn decode(
    units: &[Unit],
    input: &StorageReadControl,
    registry: &MemoryCaptureRegistry,
) -> StorageBackendResult<MemoryTableData> {
    let mut value = builder(input, registry);
    for (unit, bytes) in units {
        value = value.read(unit, &mut bytes.as_slice())?;
    }
    value.finish()
}

#[test]
fn assembled_table_data_preserves_all_vector_kinds_text_and_evaluated_values_after_input_retirement(
) {
    for kind in 0..4 {
        let units = fixture(kind);
        let input = StorageReadControl::with_limit(32 << 20);
        let registry = MemoryCaptureRegistry::new();
        let mut data = decode(&units, &input, &registry).unwrap();
        drop(units);
        input.cancellation().cancel();
        let query = StorageReadControl::with_limit(1 << 20);
        assert_eq!(
            data.documents.get_field(7, "body").unwrap(),
            Some(Value::Str("original words".into()))
        );
        assert_eq!(
            data.text
                .get_posting_list("body", "original")
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [7]
        );
        assert_eq!(
            data.vectors
                .get("embedding")
                .unwrap()
                .search_knn(&[1.0, 0.0], 1)
                .unwrap()
                .iter()
                .next()
                .unwrap()
                .doc_id,
            7
        );
        let key = ValueIndexKey::Index("opaque\0key".into());
        assert_eq!(
            data.values
                .get(&key)
                .unwrap()
                .stored_value(7, &query)
                .unwrap()
                .as_deref(),
            Some(&Value::Null)
        );
        assert_eq!(
            data.values
                .get(&key)
                .unwrap()
                .stored_value(9, &query)
                .unwrap()
                .as_deref(),
            Some(&Value::Str("stored expression".into()))
        );
        data.documents
            .put(
                8,
                BTreeMap::from([("body".into(), Value::Str("new".into()))]),
            )
            .unwrap();
        data.text
            .add_document(8, BTreeMap::from([("body".into(), "new".into())]))
            .unwrap();
        assert_eq!(
            data.text
                .get_posting_list("body", "new")
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [8]
        );
        assert!(input.memory().used() > 0);
        drop((data, registry));
        assert_eq!(input.memory().used(), 0, "vector kind {kind}");
    }
}

#[test]
fn table_data_component_failures_retire_every_preceding_owner() {
    let units = fixture(0);
    for failed in 0..units.len() {
        let input = StorageReadControl::with_limit(32 << 20);
        let registry = MemoryCaptureRegistry::new();
        let mut data = builder(&input, &registry);
        for (unit, bytes) in &units[..failed] {
            data = data.read(unit, &mut bytes.as_slice()).unwrap();
        }
        let (unit, bytes) = &units[failed];
        let mut corrupt = bytes.clone();
        corrupt.push(0);
        assert!(
            data.read(unit, &mut corrupt.as_slice()).is_err(),
            "component {failed}"
        );
        assert_eq!(input.memory().used(), 0, "component {failed}");
    }
    let input = StorageReadControl::with_limit(32 << 20);
    let registry = MemoryCaptureRegistry::new();
    assert!(decode(&units[..units.len() - 1], &input, &registry).is_err());
    assert_eq!(input.memory().used(), 0);
    let data = builder(&input, &registry)
        .read(&units[0].0, &mut units[0].1.as_slice())
        .unwrap();
    input.cancellation().cancel();
    assert!(data.finish().is_err());
    assert_eq!(input.memory().used(), 0);
}
