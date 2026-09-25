//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{parse_diskann_index_options, DiskANNIndexOptions};

fn options(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

#[test]
fn omitted_options_remain_unresolved_and_explicit_values_survive() {
    assert_eq!(
        parse_diskann_index_options(&[]).unwrap(),
        DiskANNIndexOptions::default()
    );
    let parsed = parse_diskann_index_options(&options(&[
        ("MAX_DEGREE", "12"),
        ("build_list_size", "96"),
        ("search_list_size", "64"),
        ("alpha", "1.25"),
        ("beam_width", "4"),
        ("pq_bytes", "7"),
        ("seed", "18446744073709551615"),
    ]))
    .unwrap();
    assert_eq!(
        parsed,
        DiskANNIndexOptions {
            max_degree: Some(12),
            build_list_size: Some(96),
            search_list_size: Some(64),
            alpha: Some(1.25),
            beam_width: Some(4),
            pq_bytes: Some(7),
            seed: Some(u64::MAX),
        }
    );
    let unresolved = parse_diskann_index_options(&options(&[("seed", "0")])).unwrap();
    assert_eq!(unresolved.seed, Some(0));
    assert_eq!(unresolved.pq_bytes, None);
}

#[test]
fn duplicate_unknown_and_cross_algorithm_options_are_rejected() {
    for key in [
        "max_degree",
        "build_list_size",
        "search_list_size",
        "alpha",
        "beam_width",
        "pq_bytes",
        "seed",
    ] {
        let input = vec![(key.into(), "2".into()), (key.to_uppercase(), "3".into())];
        assert!(parse_diskann_index_options(&input)
            .unwrap_err()
            .to_string()
            .contains("duplicates"));
    }
    for key in [
        "m",
        "ef_search",
        "lists",
        "nprobe",
        "max-degree",
        "format_revision",
        "algorithm_revision",
        "unknown",
    ] {
        assert!(parse_diskann_index_options(&options(&[(key, "2")]))
            .unwrap_err()
            .to_string()
            .contains("not supported"));
    }
}

#[test]
fn numeric_options_reject_nonfinite_signed_and_out_of_range_values() {
    for key in [
        "max_degree",
        "build_list_size",
        "search_list_size",
        "beam_width",
        "pq_bytes",
    ] {
        for value in ["0", "-1", "1.5", "18446744073709551616", "NaN"] {
            assert!(
                parse_diskann_index_options(&options(&[(key, value)])).is_err(),
                "{key}={value}"
            );
        }
    }
    for value in ["NaN", "inf", "-inf", "1e999", "not-a-number"] {
        assert!(parse_diskann_index_options(&options(&[("alpha", value)])).is_err());
    }
    for value in ["-1", "18446744073709551616", "1.5"] {
        assert!(parse_diskann_index_options(&options(&[("seed", value)])).is_err());
    }
}

#[test]
fn diskann_descriptors_do_not_enable_public_access_method_routing() {
    let statement = crate::compiler::compile(
        "CREATE INDEX items_diskann ON items USING diskann (embedding) WITH (alpha = 1.2, pq_bytes = 7)",
    ).unwrap().remove(0);
    let crate::ast::Statement::CreateIndex(statement) = statement else {
        panic!("expected an index statement");
    };
    let parsed = parse_diskann_index_options(&statement.options).unwrap();
    assert_eq!((parsed.alpha, parsed.pq_bytes), (Some(1.2), Some(7)));
    let error = super::super::index_access_method(&statement).unwrap_err();
    assert!(error
        .to_string()
        .contains("access method `diskann` is not supported"));
}

#[test]
fn existing_ivf_hnsw_options_keep_their_aliases_and_rejections() {
    let ivf = super::super::parse_ivf_index_options(&options(&[("lists", "9"), ("probes", "3")]))
        .unwrap();
    assert_eq!((ivf.nlist, ivf.nprobe), (Some(9), Some(3)));
    let hnsw =
        super::super::parse_hnsw_index_options(&options(&[("ef-search", "48"), ("seed", "0")]))
            .unwrap();
    assert_eq!((hnsw.ef_search, hnsw.seed), (Some(48), Some(0)));
    assert!(
        super::super::parse_ivf_index_options(&options(&[("lists", "2"), ("nlist", "4")])).is_err()
    );
    assert!(super::super::parse_hnsw_index_options(&options(&[
        ("ef_search", "2"),
        ("ef-search", "4")
    ]))
    .is_err());
    assert!(super::super::parse_ivf_index_options(&options(&[("pq_bytes", "4")])).is_err());
    assert!(super::super::parse_hnsw_index_options(&options(&[("alpha", "1.2")])).is_err());
}
