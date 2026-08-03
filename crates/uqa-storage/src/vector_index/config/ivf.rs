//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF parameters and persisted-catalog decoding.

use std::collections::BTreeMap;

use super::parsing::{read_positive_usize, reject_unknown_parameters};
use crate::{StorageBackendError, StorageBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IVFIndexParams {
    pub nlist: usize,
    pub nprobe: usize,
    pub train_threshold: usize,
}

impl Default for IVFIndexParams {
    fn default() -> Self {
        Self {
            nlist: 100,
            nprobe: 10,
            train_threshold: 256,
        }
    }
}

impl IVFIndexParams {
    pub fn validate(self) -> StorageBackendResult<Self> {
        for (name, value) in [
            ("nlist", self.nlist),
            ("nprobe", self.nprobe),
            ("train_threshold", self.train_threshold),
        ] {
            if value == 0 {
                return Err(StorageBackendError::Other(format!(
                    "IVF parameter `{name}` must be greater than zero"
                )));
            }
        }
        Ok(self)
    }

    pub fn from_catalog_map(parameters: &BTreeMap<String, String>) -> StorageBackendResult<Self> {
        reject_unknown_parameters(
            parameters,
            &[
                "lists",
                "nlist",
                "probes",
                "nprobe",
                "train_threshold",
                "train-threshold",
                "min_train",
            ],
            "IVF",
        )?;
        let defaults = Self::default();
        Self {
            nlist: read_positive_usize(parameters, &["lists", "nlist"], defaults.nlist, "IVF")?,
            nprobe: read_positive_usize(parameters, &["probes", "nprobe"], defaults.nprobe, "IVF")?,
            train_threshold: read_positive_usize(
                parameters,
                &["train_threshold", "train-threshold", "min_train"],
                defaults.train_threshold,
                "IVF",
            )?,
        }
        .validate()
    }
}
