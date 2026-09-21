//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public and directly bound path handles preserve participant attribution across the three providers.

use super::{finish, fixtures, pivot, prepare, Edge};

#[test]
fn path_index_handles_preserve_graph_dependencies_through_cached_and_transaction_views() {
    for cached in [true, false] {
        for through_engine in [true, false] {
            let (_directory, sessions) = fixtures();
            for a in sessions {
                prepare(&a);
                let sequence = vec!["likes".into()];
                a.engine
                    .build_path_index("p", "g", std::slice::from_ref(&sequence))
                    .unwrap();
                if !cached {
                    a.catalog.clear_path_index_data("g::p").unwrap();
                }
                let b = a.sibling();
                a.begin();
                b.begin();
                let index = if through_engine {
                    a.engine.get_path_index("p", "g").unwrap().unwrap()
                } else {
                    assert_eq!(
                        a.catalog
                            .path_index_data_is_current("g::p", "[[\"likes\"]]")
                            .unwrap(),
                        cached,
                    );
                    uqa_graph::PathIndex::open_persistent(
                        a.catalog.clone(),
                        a.backend.clone(),
                        "g::p",
                        "g",
                        std::slice::from_ref(&sequence),
                    )
                    .unwrap()
                };
                assert!(index.lookup(&sequence).unwrap().unwrap().is_empty());
                pivot(&a, &b);
                b.engine
                    .add_graph_edge(Edge::new(11, 1, 2, "likes"), "g")
                    .unwrap();
                finish(&a, &b, true);
            }
        }
    }
}

#[test]
fn retained_path_index_handles_keep_original_dependencies_across_the_three_providers() {
    for cached in [true, false] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a);
            let sequence = vec!["likes".into()];
            a.engine
                .build_path_index("p", "g", std::slice::from_ref(&sequence))
                .unwrap();
            if !cached {
                a.catalog.clear_path_index_data("g::p").unwrap();
            }
            let b = a.sibling();
            a.begin();
            b.begin();
            let index = uqa_graph::PathIndex::open_persistent(
                a.catalog.clone(),
                a.backend.clone(),
                "g::p",
                "g",
                std::slice::from_ref(&sequence),
            )
            .unwrap();
            let retained = a
                .backend
                .open_retained_read_session(&uqa_core::CancellationToken::new())
                .unwrap();
            let index = index
                .rebind_persistent(retained.catalog, retained.backend)
                .unwrap();
            assert!(index.lookup(&sequence).unwrap().unwrap().is_empty());
            pivot(&a, &b);
            b.engine
                .add_graph_edge(Edge::new(11, 1, 2, "likes"), "g")
                .unwrap();
            finish(&a, &b, true);
            assert!(index.lookup(&sequence).is_err());
        }
    }
}
