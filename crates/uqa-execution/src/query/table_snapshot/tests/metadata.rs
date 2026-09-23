//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn empty_capture_shares_default_and_retains_selected_field_metadata() {
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let text_fields = ["text_".repeat(8192)];
    let dimensions = BTreeMap::from([("vector_".repeat(8192), 2)]);
    let mut selected = schema(&[], &index);
    selected.text_fields = &text_fields;
    selected.vector_dimensions = &dimensions;
    let control = StorageReadControl::with_limit(1 << 20);
    let view = empty(&selected, &control).unwrap();
    assert!(std::ptr::eq(view.text.analyzer(), index.analyzer()));
    let bytes = control.memory().used();
    assert!(bytes > text_fields[0].len() + dimensions.first_key_value().unwrap().0.len());
    let text = view.text.snapshot().unwrap();
    let documents = view.documents.snapshot().unwrap();
    let vector = view.vectors.values().next().unwrap().snapshot().unwrap();
    drop(view);
    assert!(control.memory().used() > 0);
    assert!(control.memory().used() < bytes);
    drop((text, documents));
    assert!(control.memory().used() > dimensions.first_key_value().unwrap().0.len());
    drop(vector);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn rejected_metadata_capture_unwinds_names_builders_and_default_owners() {
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let dimensions = BTreeMap::from([("a".into(), 2), ("z".repeat(1 << 16), 2)]);
    let mut selected = schema(&[], &index);
    selected.vector_dimensions = &dimensions;
    let control = StorageReadControl::with_limit(4096);
    let error = empty(&selected, &control).err().unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(control.memory().peak() > 0);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    let error = empty(&selected, &control).err().unwrap();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn vector_catalog_selection_borrows_registered_index_names() {
    let fields: BTreeMap<FieldName, Box<dyn VectorIndex>> = BTreeMap::from([(
        "vector_".repeat(8192),
        Box::new(
            uqa_storage::vector_index::RetainedVectorIndexBuilder::new(
                3,
                &StorageReadControl::with_limit(4096),
            )
            .finish()
            .unwrap(),
        ) as Box<dyn VectorIndex>,
    )]);
    let index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let mut selected = schema(&[], &index);
    selected.vector_dimensions = &fields;
    let control = StorageReadControl::with_limit(4096);
    let projected = index_fields(&selected, &control).unwrap();
    assert!(std::ptr::eq(
        projected[0],
        fields.first_key_value().unwrap().0.as_str()
    ));
    assert!(control.memory().used() < projected[0].len());
    drop(projected);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn reconstructed_default_keeps_the_resolved_revision_after_resources_disappear() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "seed => retained\n").unwrap();
    let analyzer = uqa_analysis::Analyzer::new(
        uqa_analysis::tokenizer::Tokenizer::Whitespace,
        vec![uqa_analysis::token_filter::TokenFilter::Synonym {
            synonyms: BTreeMap::new(),
            synonyms_path: Some(path.to_str().unwrap().into()),
        }],
        Vec::new(),
    );
    let index = MemoryInvertedIndex::new(analyzer);
    let revision = index.index_analyzer_revision("original").unwrap();
    std::fs::remove_file(path).unwrap();
    let selected = schema(&[], &index);
    let control = StorageReadControl::with_limit(4096);
    let view = empty(&selected, &control).unwrap();
    let nested = view.text.snapshot().unwrap();
    drop((view, selected));
    drop(index);
    assert!(Arc::ptr_eq(
        &nested.index_analyzer_revision("unbound").unwrap(),
        &revision
    ));
    assert!(Arc::ptr_eq(
        &nested.search_analyzer_revision("unbound").unwrap(),
        &revision
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn direct_vector_capture_admits_names_and_keeps_them_through_nested_readers() {
    let source_control = StorageReadControl::with_limit(4096);
    let field = "retained_vector_".repeat(8192);
    let source: BTreeMap<FieldName, Box<dyn VectorIndex>> = BTreeMap::from([(
        field.clone(),
        Box::new(
            RetainedVectorIndexBuilder::new(2, &source_control)
                .finish()
                .unwrap(),
        ) as Box<dyn VectorIndex>,
    )]);
    let rejected = StorageReadControl::with_limit(4096);
    let error = retain_vector_indexes(&source, &rejected).err().unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(rejected.memory().used(), 0);
    let control = StorageReadControl::with_limit(1 << 20);
    let captured = retain_vector_indexes(&source, &control).unwrap();
    let bytes = control.memory().used();
    assert!(bytes > field.len());
    let nested = captured[&field].snapshot().unwrap().snapshot().unwrap();
    assert_eq!(control.memory().used(), bytes);
    drop((source, captured));
    assert_eq!(nested.dimensions(), 2);
    assert!(control.memory().used() > field.len());
    assert!(control.memory().used() < bytes);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source_control.memory().used(), 0);
}
