//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{analyzer_image::write_analyzer_image, StorageBackendError};
use uqa_analysis::{Analyzer, Tokenizer};

fn bytes(image: &AnalyzerImageSnapshot) -> Vec<u8> {
    let mut output = Vec::new();
    image
        .write_image(&mut output, &StorageReadControl::with_limit(0))
        .unwrap();
    output
}

fn expected(revision: &BoundAnalyzerRevision) -> Vec<u8> {
    let mut output = Vec::new();
    write_analyzer_image(
        &revision.compiled,
        &mut output,
        &StorageReadControl::with_limit(0),
    )
    .unwrap();
    output
}

#[test]
fn field_analyzer_capture_preserves_independent_sides_names_owner_and_last_phase() {
    let index = uqa_analysis::standard_analyzer("english")
        .compile()
        .unwrap();
    let search = Analyzer::new(Tokenizer::Keyword, Vec::new(), Vec::new())
        .compile()
        .unwrap();
    let mut source = BTreeMap::new();
    for (name, owner, last_phase) in [
        ("index", AnalyzerBindingOwner::Field, AnalyzerPhase::Index),
        ("search", AnalyzerBindingOwner::Field, AnalyzerPhase::Search),
        ("both", AnalyzerBindingOwner::Gin, AnalyzerPhase::Both),
    ] {
        let index_name = (last_phase != AnalyzerPhase::Search).then(|| "historical".into());
        let search_name = (last_phase != AnalyzerPhase::Index).then(|| "historical".into());
        source.insert(
            ("\"schema.with.dot\".\"table\"".into(), name.into()),
            FieldAnalyzerBinding {
                index: BoundAnalyzerRevision {
                    name: index_name,
                    compiled: index.clone(),
                },
                search: BoundAnalyzerRevision {
                    name: search_name,
                    compiled: if last_phase == AnalyzerPhase::Both {
                        index.clone()
                    } else {
                        search.clone()
                    },
                },
                owner,
                last_phase,
            },
        );
    }
    source.insert(
        ("unassigned".into(), "field".into()),
        FieldAnalyzerBinding::unassigned(index.clone(), search.clone()),
    );
    for value in source.values() {
        value.validate().unwrap();
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let saved = fields(&source, &control).unwrap();
    assert_eq!(saved.len(), source.len());
    for ((name, saved), (original_name, original)) in saved.iter().zip(&source) {
        assert_eq!(name, original_name);
        assert_eq!(saved.index.name, original.index.name);
        assert_eq!(saved.search.name, original.search.name);
        assert_eq!(saved.owner, original.owner);
        assert_eq!(saved.last_phase, original.last_phase);
        assert_eq!(bytes(&saved.index.image), expected(&original.index));
        assert_eq!(bytes(&saved.search.image), expected(&original.search));
        if original.owner == AnalyzerBindingOwner::Field {
            assert_ne!(bytes(&saved.index.image), bytes(&saved.search.image));
        }
    }
    let child = saved[0].1.search.image.clone();
    drop((saved, source, index, search));
    assert!(control.memory().used() > 0);
    control.cancellation().cancel();
    let mut output = Vec::new();
    assert!(matches!(
        child.write_image(&mut output, &StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(output.is_empty());
    drop(child);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn field_analyzer_capture_releases_prior_sides_when_a_late_name_is_denied_or_cancelled() {
    let compiled = Analyzer::default().compile().unwrap();
    let binding = FieldAnalyzerBinding::unassigned(compiled.clone(), compiled);
    let source = BTreeMap::from([
        (("first".into(), "field".into()), binding.clone()),
        (("last".repeat(8192), "field".into()), binding),
    ]);
    let control = StorageReadControl::with_limit(4096);
    assert!(matches!(
        fields(&source, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        fields(&source, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
