//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::index_stats;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    rc::Rc,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_operators::OperatorTree;
use uqa_sql::SQLError;
mod fixture;
use fixture::{Catalog, Inputs};

fn term(query: impl Into<String>) -> OperatorTree {
    OperatorTree::Term {
        query: query.into(),
        field: Some("body".into()),
        scoring: None,
        top_k: None,
    }
}
fn vector(query: Vec<f32>, field: &str) -> OperatorTree {
    OperatorTree::KNN {
        query_vector: query,
        field: field.into(),
        k: 1,
    }
}
fn text_address(tree: &OperatorTree) -> (usize, usize) {
    let OperatorTree::Term { field, query, .. } = tree else {
        unreachable!()
    };
    (
        field.as_ref().unwrap().as_ptr() as usize,
        query.as_ptr() as usize,
    )
}

#[test]
fn text_statistics_borrow_the_source_and_retain_only_the_published_key() {
    let catalog = Catalog(Rc::new(Inputs::new()));
    let tree = term("x".repeat(4 * 1024 * 1024));
    let memory = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(index_stats(&catalog, "docs", &tree, &control).unwrap());
    });
    let output = output.unwrap();
    assert_eq!(*catalog.0.text.borrow(), [text_address(&tree)]);
    assert_eq!(memory.used(), output.reserved_bytes());
    assert!(
        allocation.bytes_total < 4 * 1024 * 1024 + 4096,
        "{allocation:?}"
    );
    let OperatorTree::Term { query, .. } = tree else {
        unreachable!()
    };
    assert_eq!(output.doc_freq("body", &query), 7);
    assert_eq!(catalog.0.frequencies.get(), 2);
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn unusable_or_unmaterialized_vector_inputs_need_no_coordinate_copy() {
    let inputs = Inputs {
        dimensions: 1024 * 1024,
        ..Inputs::new()
    };
    let catalog = Catalog(Rc::new(inputs));
    let tree = OperatorTree::Union(vec![
        vector(vec![0.5; 1024 * 1024], "embedding"),
        vector(vec![0.5; 1024 * 1024], "missing"),
        vector(vec![f32::NAN; 1024 * 1024], "embedding"),
    ]);
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let allocation = allocation_counter::measure(|| {
        let stats = index_stats(&catalog, "docs", &tree, &control).unwrap();
        assert_eq!(stats.dimensions, 1024 * 1024);
        assert_eq!(stats.reserved_bytes(), 0);
    });
    assert!(allocation.bytes_total < 4096, "{allocation:?}");
    let OperatorTree::Union(children) = &tree else {
        unreachable!()
    };
    let OperatorTree::KNN {
        field,
        query_vector,
        ..
    } = &children[0]
    else {
        unreachable!()
    };
    assert_eq!(
        *catalog.0.vectors.borrow(),
        [(field.as_ptr() as usize, query_vector.as_ptr() as usize)]
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn statistics_keep_text_preorder_zip_clamping_and_guard_order() {
    let catalog = Catalog(Rc::new(Inputs::new()));
    let tree = OperatorTree::Union(vec![
        vector(vec![1.0, 0.0], "v"),
        term("first"),
        OperatorTree::Phrase {
            query: "phrase".into(),
            field: Some("body".into()),
            scoring: None,
        },
        OperatorTree::BayesianMatchWithPrior {
            field: "body".into(),
            query: "prior".into(),
            prior_field: "importance".into(),
            mode: uqa_core::retrieval::ExternalPriorMode::Authority,
        },
        OperatorTree::MultiFieldSearch {
            fields: vec!["a".into()],
            queries: vec!["paired".into(), "ignored".into()],
            weights: None,
        },
        OperatorTree::Term {
            query: "all".into(),
            field: None,
            scoring: None,
            top_k: None,
        },
        OperatorTree::VectorSimilarity {
            query_vector: vec![0.0, 1.0],
            field: "v".into(),
            threshold: 0.5,
        },
        OperatorTree::CalibratedVectorMatch {
            query_vector: vec![1.0, 1.0],
            field: "v".into(),
            k: 1,
            threshold: None,
        },
    ]);
    let stats = index_stats(&catalog, "docs", &tree, &ProductionControl::uncontrolled()).unwrap();
    for (field, query) in [
        ("body", "first"),
        ("body", "phrase"),
        ("body", "prior"),
        ("a", "paired"),
        ("_default", "all"),
    ] {
        assert_eq!(stats.doc_freq(field, query), 7);
    }
    assert_eq!(stats.doc_freq("a", "ignored"), 0);
    assert_eq!(catalog.0.analyses.get(), 4);
    assert_eq!(
        *catalog.0.events.borrow(),
        [
            "text open",
            "analyze",
            "frequency",
            "frequency",
            "analyze",
            "frequency",
            "frequency",
            "analyze",
            "frequency",
            "frequency",
            "analyze",
            "frequency",
            "frequency",
            "all fields",
            "text close",
            "vectors open",
            "dimensions",
            "physical",
            "dimensions",
            "physical",
            "dimensions",
            "physical",
            "vectors close",
            "table close"
        ]
    );
}

#[test]
fn typed_callback_errors_precede_cancellation_and_skip_large_suffixes() {
    for state in ["53200", "40001", "57014"] {
        let memory = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let inputs = Inputs {
            error: std::cell::RefCell::new(Some(SQLError::Routine {
                sqlstate: state.into(),
                message: "callback failure".into(),
            })),
            cancel: Some(invoking.clone()),
            ..Inputs::new()
        };
        let catalog = Catalog(Rc::new(inputs));
        let tree = OperatorTree::Union(vec![
            term("first"),
            term("error"),
            term("x".repeat(4 * 1024 * 1024)),
            vector(vec![0.0; 1024 * 1024], "v"),
        ]);
        let control = ProductionControl::new(&memory, &original, &invoking);
        let sibling = memory.reserve(123).unwrap();
        let allocation = allocation_counter::measure(|| {
            let error = index_stats(&catalog, "docs", &tree, &control).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state));
            assert!(error.to_string().contains("callback failure"));
        });
        assert!(allocation.bytes_total < 4096, "{allocation:?}");
        assert_eq!(catalog.0.analyses.get(), 2);
        assert_eq!(catalog.0.frequencies.get(), 2);
        assert_eq!(memory.used(), sibling.bytes());
        assert!(catalog
            .0
            .events
            .borrow()
            .ends_with(&["text close", "table close"]));
        assert!(catalog.0.vectors.borrow().is_empty());
    }
}

#[test]
fn either_cancellation_scope_releases_partial_statistics_and_readers() {
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let inputs = Inputs {
            cancel: Some(if cancel_original {
                original.clone()
            } else {
                invoking.clone()
            }),
            ..Inputs::new()
        };
        let catalog = Catalog(Rc::new(inputs));
        let tree = OperatorTree::Union(vec![term("first"), term("cancel"), term("unreached")]);
        let error = index_stats(
            &catalog,
            "docs",
            &tree,
            &ProductionControl::new(&memory, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(catalog.0.analyses.get(), 2);
        assert_eq!(catalog.0.frequencies.get(), 2);
        assert_eq!(memory.used(), 0);
        assert!(catalog
            .0
            .events
            .borrow()
            .ends_with(&["text close", "table close"]));
    }
}

#[test]
fn vector_callback_unwind_releases_text_statistics_and_both_guards() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let inputs = Inputs {
        unwind_vector: true,
        ..Inputs::new()
    };
    let catalog = Catalog(Rc::new(inputs));
    let tree = OperatorTree::Union(vec![term("first"), vector(vec![1.0, 0.0], "v")]);
    let result = catch_unwind(AssertUnwindSafe(|| {
        index_stats(
            &catalog,
            "docs",
            &tree,
            &ProductionControl::new(&memory, &token, &token),
        )
    }));
    assert!(result.is_err());
    assert_eq!(memory.used(), 0);
    assert!(catalog
        .0
        .events
        .borrow()
        .ends_with(&["physical", "vectors close", "table close"]));
    assert!(catalog.0.events.borrow().contains(&"text close"));
}

#[test]
fn physical_statistics_reuse_exact_queries_and_retain_the_coordinates() {
    let inputs = Inputs {
        physical: Some(fixture::physical()),
        ..Inputs::new()
    };
    let catalog = Catalog(Rc::new(inputs));
    let tree = OperatorTree::Union(vec![
        vector(vec![1.0, 0.0], "v"),
        vector(vec![1.0, 0.0], "v"),
        vector(vec![1.0, -0.0], "v"),
    ]);
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let stats = index_stats(
        &catalog,
        "docs",
        &tree,
        &ProductionControl::new(&memory, &token, &token),
    )
    .unwrap();
    assert!(stats.reserved_bytes() > 0);
    assert_eq!(memory.used(), stats.reserved_bytes());
    for query in [[1.0, 0.0], [1.0, -0.0]] {
        assert_eq!(
            stats.diskann_query("v", &query),
            catalog.0.physical.as_ref()
        );
    }
    let OperatorTree::Union(children) = &tree else {
        unreachable!()
    };
    let expected: Vec<_> = [0, 2]
        .into_iter()
        .map(|index| {
            let OperatorTree::KNN {
                field,
                query_vector,
                ..
            } = &children[index]
            else {
                unreachable!()
            };
            (field.as_ptr() as usize, query_vector.as_ptr() as usize)
        })
        .collect();
    assert_eq!(*catalog.0.vectors.borrow(), expected);
    drop(tree);
    assert!(stats.diskann_query("v", &[1.0, -0.0]).is_some());
    drop(stats);
    assert_eq!(memory.used(), 0);
}
