//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW parameters and persisted-catalog decoding.

use std::collections::BTreeMap;

use super::parsing::{read_positive_usize, read_u64, reject_unknown_parameters};
use crate::{StorageBackendError, StorageBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HNSWIndexParams {
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub rebuild_threshold: usize,
    pub seed: u64,
}

impl Default for HNSWIndexParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 64,
            ef_search: 40,
            rebuild_threshold: 1024,
            seed: 0x05ee_d5ee_dd15_ca11,
        }
    }
}

impl HNSWIndexParams {
    pub fn validate(self) -> StorageBackendResult<Self> {
        if self.m < 2 {
            return Err(StorageBackendError::Other(
                "HNSW parameter `m` must be at least 2".into(),
            ));
        }
        if self.ef_construction < self.m {
            return Err(StorageBackendError::Other(format!(
                "HNSW parameter `ef_construction` must be at least m ({})",
                self.m
            )));
        }
        if self.ef_search == 0 {
            return Err(StorageBackendError::Other(
                "HNSW parameter `ef_search` must be greater than zero".into(),
            ));
        }
        if self.rebuild_threshold == 0 {
            return Err(StorageBackendError::Other(
                "HNSW parameter `rebuild_threshold` must be greater than zero".into(),
            ));
        }
        Ok(self)
    }

    pub fn from_catalog_map(parameters: &BTreeMap<String, String>) -> StorageBackendResult<Self> {
        reject_unknown_parameters(
            parameters,
            &[
                "m",
                "ef_construction",
                "ef-construction",
                "ef_search",
                "ef-search",
                "rebuild_threshold",
                "rebuild-threshold",
                "seed",
            ],
            "HNSW",
        )?;
        let defaults = Self::default();
        Self {
            m: read_positive_usize(parameters, &["m"], defaults.m, "HNSW")?,
            ef_construction: read_positive_usize(
                parameters,
                &["ef_construction", "ef-construction"],
                defaults.ef_construction,
                "HNSW",
            )?,
            ef_search: read_positive_usize(
                parameters,
                &["ef_search", "ef-search"],
                defaults.ef_search,
                "HNSW",
            )?,
            rebuild_threshold: read_positive_usize(
                parameters,
                &["rebuild_threshold", "rebuild-threshold"],
                defaults.rebuild_threshold,
                "HNSW",
            )?,
            seed: read_u64(parameters, &["seed"], defaults.seed, "HNSW")?,
        }
        .validate()
    }
}
