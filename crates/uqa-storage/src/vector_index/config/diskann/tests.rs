//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DiskANNAlpha, DiskANNIndexParams};

mod fixtures;

#[test]
fn defaults_resolve_pq_width_from_the_target_dimension() {
    for (dimensions, pq_bytes) in [(1, 1), (7, 7), (32, 32), (33, 32), (1536, 32)] {
        let params = DiskANNIndexParams::for_dimensions(dimensions).unwrap();
        assert_eq!(params.pq_bytes, pq_bytes);
        assert_eq!(params.max_degree, 64);
        assert_eq!(params.build_list_size, 128);
        assert_eq!(params.search_list_size, 64);
        assert_eq!(params.beam_width, 4);
        assert_eq!(params.seed, 42);
        assert_eq!(params.alpha.get(), 1.2);
    }
    assert!(DiskANNIndexParams::for_dimensions(0).is_err());
}

#[test]
fn alpha_rejects_invalid_values_and_preserves_euclidean_units() {
    for value in [
        0.0,
        -0.0,
        -1.0,
        0.999,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        1.0e155,
    ] {
        assert!(DiskANNAlpha::new(value).is_err(), "{value}");
    }
    for value in [1.0, 1.2, 2.0, 1.0e154] {
        let alpha = DiskANNAlpha::new(value).unwrap();
        assert_eq!(alpha.get().to_bits(), value.to_bits());
        assert_eq!(alpha.squared(), value * value);
    }
    assert_eq!(
        DiskANNAlpha::new("1.200".parse().unwrap()).unwrap(),
        DiskANNAlpha::default()
    );
    let alpha = DiskANNAlpha::default();
    // Unit-circle points (1,0), (3/5,4/5), (-1,0) have these exact squared distances.
    assert!(alpha.get() * 3.2 <= 4.0);
    assert!(alpha.squared() * 3.2 > 4.0);
}

#[test]
fn rejects_invalid_graph_pq_and_buffer_bounds() {
    let defaults = DiskANNIndexParams::for_dimensions(7).unwrap();
    for (params, parameter) in [
        (
            DiskANNIndexParams {
                max_degree: 1,
                ..defaults
            },
            "max_degree",
        ),
        (
            DiskANNIndexParams {
                build_list_size: 63,
                ..defaults
            },
            "build_list_size",
        ),
        (
            DiskANNIndexParams {
                search_list_size: 0,
                ..defaults
            },
            "search_list_size",
        ),
        (
            DiskANNIndexParams {
                beam_width: 0,
                ..defaults
            },
            "beam_width",
        ),
        (
            DiskANNIndexParams {
                beam_width: 65,
                ..defaults
            },
            "beam_width",
        ),
        (
            DiskANNIndexParams {
                pq_bytes: 0,
                ..defaults
            },
            "pq_bytes",
        ),
        (
            DiskANNIndexParams {
                pq_bytes: 8,
                ..defaults
            },
            "pq_bytes",
        ),
        (
            DiskANNIndexParams {
                max_degree: usize::MAX,
                build_list_size: usize::MAX,
                ..defaults
            },
            "max_degree",
        ),
        (
            DiskANNIndexParams {
                build_list_size: usize::MAX,
                ..defaults
            },
            "build_list_size",
        ),
        (
            DiskANNIndexParams {
                search_list_size: usize::MAX,
                ..defaults
            },
            "search_list_size",
        ),
    ] {
        let error = params.validate(7).unwrap_err().to_string();
        assert!(error.contains(parameter), "{error}");
        assert!(params.to_catalog_map(7).is_err());
    }
    let tiny = DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 2,
        search_list_size: 1,
        beam_width: 1,
        pq_bytes: 1,
        ..defaults
    };
    assert_eq!(tiny.validate(1).unwrap(), tiny);
}

#[test]
fn restore_requires_every_effective_setting_and_current_revision() {
    let original = DiskANNIndexParams::for_dimensions(7).unwrap();
    let persisted = original.to_catalog_map(7).unwrap();
    assert_eq!(persisted.len(), 9);
    assert_eq!(
        DiskANNIndexParams::from_catalog_map(7, &persisted).unwrap(),
        original
    );
    for key in persisted.keys() {
        let mut missing = persisted.clone();
        missing.remove(key);
        let error = DiskANNIndexParams::from_catalog_map(7, &missing)
            .unwrap_err()
            .to_string();
        assert!(error.contains(key) && error.contains("missing"), "{error}");
    }
    for key in ["format_revision", "algorithm_revision"] {
        for value in ["0", "2", "4294967296", "-1"] {
            let mut invalid = persisted.clone();
            invalid.insert(key.into(), value.into());
            assert!(DiskANNIndexParams::from_catalog_map(7, &invalid).is_err());
        }
    }
    assert!(DiskANNIndexParams::from_catalog_map(6, &persisted).is_err());
}

#[test]
fn catalog_rejects_case_duplicates_cross_algorithm_options_and_overflow() {
    let params = DiskANNIndexParams::for_dimensions(64).unwrap();
    let persisted = params.to_catalog_map(64).unwrap();
    let upper = persisted
        .iter()
        .map(|(k, v)| (k.to_uppercase(), v.clone()))
        .collect();
    assert_eq!(
        DiskANNIndexParams::from_catalog_map(64, &upper).unwrap(),
        params
    );
    for (key, value) in [
        ("ALPHA", "1.2"),
        ("m", "16"),
        ("lists", "8"),
        ("max-degree", "64"),
    ] {
        let mut invalid = persisted.clone();
        invalid.insert(key.into(), value.into());
        assert!(DiskANNIndexParams::from_catalog_map(64, &invalid).is_err());
    }
    for key in [
        "max_degree",
        "build_list_size",
        "search_list_size",
        "beam_width",
        "pq_bytes",
        "seed",
    ] {
        let mut invalid = persisted.clone();
        invalid.insert(key.into(), "18446744073709551616".into());
        assert!(DiskANNIndexParams::from_catalog_map(64, &invalid).is_err());
    }
    for value in ["NaN", "inf", "0.5", "1e155", "not-a-number"] {
        let mut invalid = persisted.clone();
        invalid.insert("alpha".into(), value.into());
        assert!(DiskANNIndexParams::from_catalog_map(64, &invalid).is_err());
    }
}

#[test]
fn catalog_round_trip_keeps_nondefault_pq_seed_and_alpha_bits() {
    let params = DiskANNIndexParams {
        alpha: DiskANNAlpha::new(f64::from_bits(1.2_f64.to_bits() + 1)).unwrap(),
        pq_bytes: 5,
        seed: u64::MAX,
        ..DiskANNIndexParams::for_dimensions(7).unwrap()
    };
    let persisted = params.to_catalog_map(7).unwrap();
    assert_eq!(
        DiskANNIndexParams::from_catalog_map(7, &persisted).unwrap(),
        params
    );
}
