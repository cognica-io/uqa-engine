//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::{HNSWIndexParams, IVFIndexParams};

#[test]
fn parses_independent_ivf_and_hnsw_parameters() {
    let ivf = IVFIndexParams::from_catalog_map(&BTreeMap::from([
        ("lists".into(), "32".into()),
        ("probes".into(), "4".into()),
    ]))
    .unwrap();
    assert_eq!(ivf.nlist, 32);
    assert_eq!(ivf.nprobe, 4);

    let hnsw = HNSWIndexParams::from_catalog_map(&BTreeMap::from([
        ("m".into(), "12".into()),
        ("ef_construction".into(), "96".into()),
        ("ef_search".into(), "48".into()),
    ]))
    .unwrap();
    assert_eq!(hnsw.m, 12);
    assert_eq!(hnsw.ef_construction, 96);
    assert_eq!(hnsw.ef_search, 48);
}

#[test]
fn rejects_invalid_hnsw_graph_bounds() {
    let error = HNSWIndexParams::from_catalog_map(&BTreeMap::from([
        ("m".into(), "16".into()),
        ("ef_construction".into(), "8".into()),
    ]))
    .unwrap_err();
    assert!(error.to_string().contains("ef_construction"));
}

#[test]
fn rejects_cross_algorithm_and_duplicate_catalog_parameters() {
    let cross = HNSWIndexParams::from_catalog_map(&BTreeMap::from([("lists".into(), "8".into())]))
        .unwrap_err();
    assert!(cross.to_string().contains("unsupported"));

    let duplicate = IVFIndexParams::from_catalog_map(&BTreeMap::from([
        ("lists".into(), "8".into()),
        ("nlist".into(), "16".into()),
    ]))
    .unwrap_err();
    assert!(duplicate.to_string().contains("duplicate"));
}
