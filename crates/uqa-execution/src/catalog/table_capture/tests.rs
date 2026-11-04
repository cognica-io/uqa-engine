//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{
    analyzer_image::{write_analyzer_image, AnalyzerImageSnapshot},
    memory_capture::MemoryCaptureRegistry,
    AnalyzerBindingOwner, AnalyzerPhase, BoundAnalyzerRevision,
};

fn binding() -> FieldAnalyzerBinding {
    let binding = FieldAnalyzerBinding {
        index: BoundAnalyzerRevision {
            name: Some("historical".into()),
            compiled: uqa_analysis::standard_analyzer("english")
                .compile()
                .unwrap(),
        },
        search: BoundAnalyzerRevision {
            name: Some("historical".into()),
            compiled: uqa_analysis::Analyzer::new(
                uqa_analysis::Tokenizer::Keyword,
                Vec::new(),
                Vec::new(),
            )
            .compile()
            .unwrap(),
        },
        owner: AnalyzerBindingOwner::Field,
        last_phase: AnalyzerPhase::Search,
    };
    binding.to_json().unwrap();
    binding
}

fn index(table: &RelationIdentity, name: &str) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new(&table.schema, name),
        index_type: "gin".into(),
        table_name: table.qualified_name(),
        columns_json: "[ \"body\" ]".into(),
        parameters_json: "{ \"n\": 1.00 }".into(),
        definition_json: Some("{ \"original\": true }".into()),
    }
}

fn fixture() -> TableMetadataCaptureSource {
    let selected = RelationIdentity::new("Quoted.Schema", "Table.\"한글");
    let temporary = RelationIdentity::new("public", "temporary_membership");
    let unlogged = RelationIdentity::new("pg_temp_literal", "unlogged");
    let mut excluded_index = index(&temporary, &"x".repeat(1 << 20));
    excluded_index.definition_json = Some("x".repeat(1 << 20));
    let mut excluded_binding = binding();
    excluded_binding.index.name = Some("x".repeat(1 << 20));
    let indexes = [
        index(&selected, "saved"),
        index(&unlogged, "saved"),
        excluded_index,
    ]
    .into_iter()
    .map(|row| (row.relation.clone(), row))
    .collect();
    let analyzers = BTreeMap::from([
        ((selected.qualified_name(), "body".into()), binding()),
        (
            (temporary.qualified_name(), "x".repeat(1 << 20)),
            excluded_binding,
        ),
    ]);
    TableMetadataCaptureSource::new(
        [
            (selected, RelationPersistence::Permanent),
            (temporary, RelationPersistence::Temporary),
            (unlogged, RelationPersistence::Unlogged),
        ],
        Arc::new(indexes),
        Arc::new(analyzers),
    )
}

fn bytes(image: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_image(&mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    bytes
}

#[test]
fn table_metadata_capture_selects_original_persistence_before_names_and_payloads() {
    let source = fixture();
    let control = StorageReadControl::with_limit(16 << 10);
    let expected = source
        .analyzers
        .values()
        .next()
        .map(|binding| {
            [&binding.index, &binding.search].map(|side| {
                let mut bytes = Vec::new();
                write_analyzer_image(
                    &side.compiled,
                    &mut bytes,
                    &StorageReadControl::with_limit(0),
                )
                .unwrap();
                bytes
            })
        })
        .unwrap();
    let mut captured = None;
    let allocated =
        allocation_counter::measure(|| captured = Some(source.capture(&control).unwrap()));
    let captured = captured.unwrap();
    let mut selected_only = fixture();
    Arc::make_mut(&mut selected_only.indexes)
        .retain(|_, row| row.table_name != "public.temporary_membership");
    Arc::make_mut(&mut selected_only.analyzers)
        .retain(|(table, _), _| table != "public.temporary_membership");
    let other_control = StorageReadControl::with_limit(16 << 10);
    let mut other = None;
    let without_temporary = allocation_counter::measure(|| {
        other = Some(selected_only.capture(&other_control).unwrap());
    });
    assert_eq!(allocated.bytes_current, without_temporary.bytes_current);
    assert_eq!(allocated.bytes_total, without_temporary.bytes_total);
    assert_eq!(allocated.count_total, without_temporary.count_total);
    assert_eq!(control.memory().used(), other_control.memory().used());
    drop((other, selected_only));
    assert_eq!(other_control.memory().used(), 0);
    assert_eq!(captured.indexes.len(), 2);
    for (name, saved) in captured.indexes.iter() {
        let original = &source.indexes[name];
        assert_eq!(saved.relation, original.relation);
        assert_eq!(saved.table_name, original.table_name);
        assert_eq!(saved.index_type, original.index_type);
        assert_eq!(saved.columns_json, original.columns_json);
        assert_eq!(saved.parameters_json, original.parameters_json);
        assert_eq!(saved.definition_json, original.definition_json);
    }
    drop(source);
    assert_eq!(captured.analyzers.len(), 1);
    let (name, saved) = &captured.analyzers[0];
    assert_eq!(
        name,
        &("\"Quoted.Schema\".\"Table.\"\"한글\"".into(), "body".into())
    );
    assert_eq!(saved.index.name.as_deref(), Some("historical"));
    assert_eq!(saved.search.name.as_deref(), Some("historical"));
    assert_eq!(saved.owner, AnalyzerBindingOwner::Field);
    assert_eq!(saved.last_phase, AnalyzerPhase::Search);
    assert_eq!(
        [bytes(&saved.index.image), bytes(&saved.search.image)],
        expected
    );
    assert_ne!(expected[0], expected[1]);
    let required = control.memory().used();
    drop(captured);
    assert_eq!(control.memory().used(), 0);
    let source = fixture();
    let denied = StorageReadControl::with_limit(required - 1);
    assert!(source.capture(&denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    let exact = StorageReadControl::with_limit(required);
    let saved = source.capture(&exact).unwrap();
    assert_eq!(exact.memory().used(), required);
    drop(saved);
    assert_eq!(exact.memory().used(), 0);
}

#[test]
fn table_metadata_capture_rejects_missing_or_invalid_original_relations_without_partial_output() {
    for invalid in ["missing", "\"unterminated"] {
        let mut source = fixture();
        let rows = Arc::make_mut(&mut source.indexes);
        rows.values_mut().next().unwrap().table_name = invalid.into();
        let control = StorageReadControl::with_limit(16 << 10);
        assert!(source.capture(&control).is_err());
        assert_eq!(control.memory().used(), 0);
        let mut source = fixture();
        Arc::make_mut(&mut source.analyzers).insert((invalid.into(), "body".into()), binding());
        assert!(source.capture(&control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn table_metadata_generations_retain_independent_controls_after_source_retirement() {
    let registry = MemoryCaptureRegistry::new();
    let cell = registry.cell(fixture()).unwrap();
    let key = cell.key().unwrap();
    let denied = StorageReadControl::with_limit(1 << 10);
    let failed = registry.capture(&denied).unwrap();
    let control = StorageReadControl::with_limit(32 << 10);
    let cut = registry.capture(&control).unwrap();
    assert!(key.retained(&cut).unwrap().is_none());
    drop(cell);
    assert!(failed.check().is_err());
    drop(failed);
    assert_eq!(denied.memory().used(), 0);
    let saved = key.captured(&cut).unwrap();
    assert_eq!(saved.get().unwrap().indexes.len(), 2);
    drop((cut, registry, key));
    let side = &saved.get().unwrap().analyzers[0].1.index.image;
    control.cancellation().cancel();
    assert!(side
        .write_image(&mut Vec::new(), &StorageReadControl::with_limit(0))
        .is_err());
    assert!(saved.get().is_err());
    drop(saved);
    assert_eq!(control.memory().used(), 0);
}
