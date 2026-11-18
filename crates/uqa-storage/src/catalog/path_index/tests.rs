//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeSet;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    CancellationToken,
};

#[test]
fn canonical_pairs_preserve_simple_keys_and_distinguish_overlapping_delimiters() {
    let parts = [
        "",
        "x",
        ":",
        "::",
        "a:b",
        "a:",
        ":b",
        "a::b",
        "a/b%\0한글",
        "@uqa-path-1/1.61.",
    ];
    let mut seen = BTreeSet::new();
    for graph in parts {
        for name in parts {
            let physical = key(graph, name);
            assert!(
                seen.insert(physical.clone()),
                "pair {graph:?},{name:?} collides"
            );
            let (g, n) = decode(&physical).unwrap();
            assert_eq!((&*g, &*n), (graph, name));
            let raw = format!("{graph}::{name}");
            let boundaries = raw.as_bytes().windows(2).filter(|s| *s == b"::").count();
            if boundaries == 1 {
                assert_eq!(physical, raw);
            } else {
                assert!(!physical.contains("::"));
            }
            assert!(belongs_to(&physical, graph, || Ok(None), |g| Ok(g == graph)).unwrap());
        }
    }
    assert_eq!(
        legacy_candidates("a:::b").collect::<Vec<_>>(),
        [("a", ":b"), ("a:", "b")]
    );
    assert!(decode("opaque").is_none());
}

#[test]
fn historical_owners_require_unique_catalog_evidence_and_scope_destructive_selection() {
    let legacy = "one::two::three";
    let both = |g: &str| Ok(["one", "one::two"].contains(&g));
    let (graph, name) = resolve(legacy, Some("one::two"), both).unwrap().unwrap();
    assert_eq!((&*graph, &*name), ("one::two", "three"));
    assert!(resolve(legacy, None, both)
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
    assert!(resolve(legacy, Some("other"), both)
        .unwrap_err()
        .to_string()
        .contains("disagrees"));
    assert!(resolve(legacy, None, |_| Ok(false))
        .unwrap_err()
        .to_string()
        .contains("missing"));
    assert_eq!(
        resolve(legacy, None, |g| Ok(g == "one"))
            .unwrap()
            .unwrap()
            .1,
        "two::three"
    );
    assert!(!belongs_to(
        legacy,
        "unrelated",
        || panic!("unrelated owner read"),
        |_| panic!("catalog lookup")
    )
    .unwrap());
    let (_, prefix) = prefixes("one::two");
    let raw_graph = format!("{prefix}ordinary");
    let masquerading = key(&raw_graph, "index");
    assert!(masquerading.starts_with(&prefix));
    assert!(masquerading.contains("::"));
    assert!(!belongs_to(
        &masquerading,
        "one::two",
        || panic!("different graph"),
        |_| panic!("different graph")
    )
    .unwrap());
    assert!(may_belong_to(&masquerading, |g| g == raw_graph));
    assert!(may_belong_to(legacy, |g| g == "one::two"));
    assert!(!may_belong_to(legacy, |g| g == "on"));
    assert!(!belongs_to(
        "opaque",
        "one",
        || panic!("opaque owner read"),
        |_| panic!("opaque graph lookup")
    )
    .unwrap());
}

struct Known(String);
impl PathIndexGraphs for Known {
    async fn contains_from(
        &self,
        graph: &str,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<bool> {
        Ok(crate::catalog::compare_names(&self.0, graph, input)
            .await?
            .is_eq())
    }
}

#[test]
fn retained_registration_resolves_the_recorded_graph_in_windows_and_transfers_both_outputs() {
    let graph = format!("{}::events", "한글".repeat(4096));
    let name = "related::paths";
    let canonical = key(&graph, name);
    let legacy = format!("{graph}::{name}");
    for physical in [legacy, canonical.clone()] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let graphs = Known(graph.clone());
        let mut reader = ImageDecoder::new(&budget, &token, |mut channel| async move {
            registration_from(&physical, Some(&graphs.0), &graphs, &mut channel.input()).await
        })
        .unwrap();
        let mut windows = 0;
        let (key, owner) = loop {
            windows += 1;
            if let Some(result) = reader
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 },
                )
                .unwrap()
            {
                break result.unwrap();
            }
        };
        assert!(windows > 64);
        assert_eq!((&*key, &*owner), (&canonical, &graph));
        drop(reader);
        assert!(key.budget().shares_allowance(&budget) && owner.budget().shares_allowance(&budget));
        assert_eq!(budget.used(), key.capacity() + owner.capacity());
        drop((key, owner));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn retained_legacy_conversion_limits_work_and_preserves_admission_and_cancellation() {
    let prefix = "한글-x".repeat(1000);
    let graph = format!("{prefix}::owner");
    let legacy = format!("{graph}::p");
    let encoded = key(&graph, "p");
    for (raw, canonical) in [(legacy.as_str(), false), (encoded.as_str(), true)] {
        for steps in [1, 37] {
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let text = raw.to_string();
            let ptr = text.as_ptr() as usize;
            let cap = text.capacity();
            let initial = Budgeted::new(text, memory.reserve(cap).unwrap());
            let g = Known(graph.clone());
            let mut reader = ImageDecoder::new(&memory, &token, move |mut channel| async move {
                canonicalized_from(initial, &g, &mut channel.input()).await
            })
            .unwrap();
            let mut count = 0;
            let result = loop {
                count += 1;
                if let Some(key) = reader
                    .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
                    .unwrap()
                {
                    break key;
                }
            };
            assert_eq!(&*result, &encoded);
            assert_eq!(ptr == result.as_ptr() as usize, canonical);
            assert!(result.reserved_bytes() >= result.capacity());
            if steps == 1 {
                assert!(count > 30);
            }
            drop((result, reader));
            assert_eq!(memory.used(), 0);
        }
    }
    for mode in 0..3 {
        let budget = MemoryBudget::new(1 << 20);
        let other = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let text = legacy.clone();
        let lease = match mode {
            0 => other.reserve(text.capacity()).unwrap(),
            1 => budget.empty_reservation(),
            _ => budget.reserve(text.capacity()).unwrap(),
        };
        let initial = Budgeted::new(text, lease);
        let graph = Known(graph.clone());
        let mut reader = ImageDecoder::new(&budget, &token, move |mut channel| async move {
            canonicalized_from(initial, &graph, &mut channel.input()).await
        })
        .unwrap();
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        if mode == 2 {
            assert!(reader
                .advance(&mut std::io::empty(), window)
                .unwrap()
                .is_none());
            token.cancel();
        }
        loop {
            match reader.advance(&mut std::io::empty(), window) {
                Err(_) => break,
                Ok(None) => (),
                Ok(Some(_)) => panic!("invalid retained path key was accepted"),
            }
        }
        drop(reader);
        assert_eq!((budget.used(), other.used()), (0, 0));
    }
}
