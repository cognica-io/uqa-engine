//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::{invalid, DiskANNAlpha, DiskANNIndexParams};
use crate::vector_index::config::parsing::{
    find_parameter, read_positive_usize, read_u64, reject_unknown_parameters,
};
use crate::StorageBackendResult;

const PARAMETERS: &[&str] = &[
    "max_degree",
    "build_list_size",
    "search_list_size",
    "alpha",
    "beam_width",
    "pq_bytes",
    "seed",
    "format_revision",
    "algorithm_revision",
];

pub(super) fn decode(
    dimensions: u32,
    parameters: &BTreeMap<String, String>,
) -> StorageBackendResult<DiskANNIndexParams> {
    reject_unknown_parameters(parameters, PARAMETERS, "DiskANN")?;
    for name in PARAMETERS {
        if find_parameter(parameters, &[*name], "DiskANN")?.is_none() {
            return Err(invalid(name, "is missing from the persisted configuration"));
        }
    }
    let size = |name| read_positive_usize(parameters, &[name], 0, "DiskANN");
    let revision = |name| {
        u32::try_from(read_u64(parameters, &[name], 0, "DiskANN")?)
            .map_err(|_| invalid(name, "exceeds the revision range"))
    };
    let (_, alpha) = find_parameter(parameters, &["alpha"], "DiskANN")?
        .ok_or_else(|| invalid("alpha", "is missing from the persisted configuration"))?;
    DiskANNIndexParams {
        max_degree: size("max_degree")?,
        build_list_size: size("build_list_size")?,
        search_list_size: size("search_list_size")?,
        alpha: DiskANNAlpha::new(
            alpha
                .parse()
                .map_err(|_| invalid("alpha", "must be a real number"))?,
        )?,
        beam_width: size("beam_width")?,
        pq_bytes: size("pq_bytes")?,
        seed: read_u64(parameters, &["seed"], 0, "DiskANN")?,
        format_revision: revision("format_revision")?,
        algorithm_revision: revision("algorithm_revision")?,
    }
    .validate(dimensions)
}

pub(super) fn encode(parameters: DiskANNIndexParams) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("max_degree".into(), parameters.max_degree.to_string()),
        (
            "build_list_size".into(),
            parameters.build_list_size.to_string(),
        ),
        (
            "search_list_size".into(),
            parameters.search_list_size.to_string(),
        ),
        ("alpha".into(), parameters.alpha.get().to_string()),
        ("beam_width".into(), parameters.beam_width.to_string()),
        ("pq_bytes".into(), parameters.pq_bytes.to_string()),
        ("seed".into(), parameters.seed.to_string()),
        (
            "format_revision".into(),
            parameters.format_revision.to_string(),
        ),
        (
            "algorithm_revision".into(),
            parameters.algorithm_revision.to_string(),
        ),
    ])
}
