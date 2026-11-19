//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::inverted_index::AnalyzerBindings;
use uqa_analysis::{Analyzer, TokenFilter};
use uqa_core::memory::Budgeted;

fn definition(value: &Analyzer, input: &StorageReadControl) -> AnalyzerDefinition {
    let bytes = serde_json::to_vec(value).unwrap();
    let memory = input.memory().reserve(bytes.capacity()).unwrap();
    AnalyzerDefinition::from_image(Budgeted::new(bytes, memory), input.cancellation()).unwrap()
}

#[test]
fn image_default_keeps_original_configuration_and_lazy_runtime_after_input_retirement() {
    let input = StorageReadControl::with_limit(1 << 20);
    let definition = definition(&uqa_analysis::whitespace_analyzer(), &input);
    let pointer = std::ptr::from_ref(definition.configuration());
    let default =
        AnalyzerDefault::from_definition(definition, AnalyzerResources::default(), &input).unwrap();
    let bindings = AnalyzerBindings::from_retained_revisions(default, [], &input).unwrap();
    assert_eq!(
        std::ptr::from_ref(bindings.default_configuration()),
        pointer
    );
    let retained = bindings
        .retained(&StorageReadControl::with_limit(0))
        .unwrap();
    let bytes = input.memory().used();
    input.cancellation().cancel();
    drop(bindings);
    assert_eq!(input.memory().used(), bytes);
    let revision = retained.index_revision("body").unwrap();
    assert_eq!(revision.analyze("Cats DOGS").unwrap(), ["cats", "dogs"]);
    assert!(Arc::ptr_eq(
        &revision,
        &retained.search_revision("other").unwrap()
    ));
    drop((revision, retained));
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn image_default_does_not_open_missing_resources_before_ordinary_first_use() {
    let directory = tempfile::tempdir().unwrap();
    let mut analyzer = uqa_analysis::whitespace_analyzer();
    analyzer.token_filters.push(TokenFilter::Synonym {
        synonyms: std::collections::BTreeMap::new(),
        synonyms_path: Some(directory.path().join("absent")),
    });
    let input = StorageReadControl::with_limit(1 << 20);
    let default = AnalyzerDefault::from_definition(
        definition(&analyzer, &input),
        AnalyzerResources::default(),
        &input,
    )
    .unwrap();
    let bindings = AnalyzerBindings::from_retained_revisions(default, [], &input).unwrap();
    input.cancellation().cancel();
    assert!(bindings.index_revision("body").is_err());
    drop(bindings);
    assert_eq!(input.memory().used(), 0);

    let source = StorageReadControl::with_limit(1 << 20);
    let rejected = StorageReadControl::with_limit(0);
    assert!(AnalyzerDefault::from_definition(
        definition(&analyzer, &source),
        AnalyzerResources::default(),
        &rejected,
    )
    .is_err());
    assert_eq!(source.memory().used(), 0);
    assert_eq!(rejected.memory().used(), 0);
}

#[test]
fn image_default_opens_independent_writable_field_maps_without_copying_configuration() {
    let control = StorageReadControl::with_limit(1 << 20);
    let definition = definition(&uqa_analysis::whitespace_analyzer(), &control);
    let pointer = std::ptr::from_ref(definition.configuration());
    let default =
        AnalyzerDefault::from_definition(definition, AnalyzerResources::default(), &control)
            .unwrap();
    let bytes = control.memory().used();
    let mut first = AnalyzerBindings::from_default(default.clone());
    let second = AnalyzerBindings::from_default(default);
    assert_eq!(std::ptr::from_ref(first.default_configuration()), pointer);
    assert_eq!(std::ptr::from_ref(second.default_configuration()), pointer);
    assert_eq!(control.memory().used(), bytes);
    control.cancellation().cancel();
    first
        .bind(
            "body",
            &uqa_analysis::keyword_analyzer(),
            super::super::AnalyzerPhase::Both,
        )
        .unwrap();
    assert_eq!(
        first
            .index_revision("body")
            .unwrap()
            .analyze("First WORD")
            .unwrap(),
        ["First WORD"]
    );
    assert_eq!(
        second
            .index_revision("body")
            .unwrap()
            .analyze("First WORD")
            .unwrap(),
        ["first", "word"]
    );
    first.remove("body");
    assert!(Arc::ptr_eq(
        &first.index_revision("body").unwrap(),
        &second.index_revision("body").unwrap()
    ));
    drop(first);
    assert_eq!(control.memory().used(), bytes);
    drop(second);
    assert_eq!(control.memory().used(), 0);
}
