//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolved `DiskANN` parameters, independent of public access-method routing.

use std::alloc::Layout;
use std::collections::BTreeMap;

use crate::{StorageBackendError, StorageBackendResult};

mod alpha;
mod catalog;

pub use alpha::DiskANNAlpha;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNIndexParams {
    pub max_degree: usize,
    pub build_list_size: usize,
    pub search_list_size: usize,
    pub alpha: DiskANNAlpha,
    pub beam_width: usize,
    pub pq_bytes: usize,
    pub seed: u64,
    pub format_revision: u32,
    pub algorithm_revision: u32,
}

impl DiskANNIndexParams {
    pub const FORMAT_REVISION: u32 = 1;
    pub const ALGORITHM_REVISION: u32 = 1;

    pub fn for_dimensions(dimensions: u32) -> StorageBackendResult<Self> {
        Self {
            max_degree: 64,
            build_list_size: 128,
            search_list_size: 64,
            alpha: DiskANNAlpha::default(),
            beam_width: 4,
            pq_bytes: usize::try_from(dimensions.min(32))
                .map_err(|_| invalid("pq_bytes", "exceeds the platform size range"))?,
            seed: 42,
            format_revision: Self::FORMAT_REVISION,
            algorithm_revision: Self::ALGORITHM_REVISION,
        }
        .validate(dimensions)
    }

    pub fn validate(self, dimensions: u32) -> StorageBackendResult<Self> {
        let dimensions = usize::try_from(dimensions)
            .map_err(|_| invalid("dimensions", "exceeds the platform size range"))?;
        if dimensions == 0 {
            return Err(invalid("dimensions", "must be greater than zero"));
        }
        if self.max_degree < 2 {
            return Err(invalid("max_degree", "must be at least 2"));
        }
        if self.build_list_size < self.max_degree {
            return Err(invalid("build_list_size", "must be at least max_degree"));
        }
        if self.search_list_size == 0 {
            return Err(invalid("search_list_size", "must be greater than zero"));
        }
        if self.beam_width == 0 || self.beam_width > self.search_list_size {
            return Err(invalid(
                "beam_width",
                "must be between 1 and search_list_size",
            ));
        }
        if self.pq_bytes == 0 || self.pq_bytes > dimensions {
            return Err(invalid("pq_bytes", "must be between 1 and dimensions"));
        }
        if self.format_revision != Self::FORMAT_REVISION {
            return Err(invalid("format_revision", "is not supported"));
        }
        if self.algorithm_revision != Self::ALGORITHM_REVISION {
            return Err(invalid("algorithm_revision", "is not supported"));
        }
        Layout::array::<u64>(self.max_degree)
            .map_err(|_| invalid("max_degree", "overflows the neighbor buffer layout"))?;
        for (name, count) in [
            ("build_list_size", self.build_list_size),
            ("search_list_size", self.search_list_size),
        ] {
            Layout::array::<(u64, f64)>(count)
                .map_err(|_| invalid(name, "overflows the candidate buffer layout"))?;
        }
        Layout::array::<f32>(dimensions)
            .map_err(|_| invalid("dimensions", "overflows the vector buffer layout"))?;
        let lookup_entries = self
            .pq_bytes
            .checked_mul(256)
            .ok_or_else(|| invalid("pq_bytes", "overflows the lookup entry count"))?;
        Layout::array::<f64>(lookup_entries)
            .map_err(|_| invalid("pq_bytes", "overflows the lookup buffer layout"))?;
        Ok(self)
    }

    pub fn from_catalog_map(
        dimensions: u32,
        parameters: &BTreeMap<String, String>,
    ) -> StorageBackendResult<Self> {
        catalog::decode(dimensions, parameters)
    }

    pub fn to_catalog_map(self, dimensions: u32) -> StorageBackendResult<BTreeMap<String, String>> {
        Ok(catalog::encode(self.validate(dimensions)?))
    }
}

fn invalid(name: &str, reason: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("DiskANN parameter `{name}` {reason}"))
}

#[cfg(test)]
mod tests;
