//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical-index selection and create-versus-restore mode.

use super::{HNSWIndexParams, IVFIndexParams};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorIndexSpec {
    BruteForce,
    IVF(IVFIndexParams),
    HNSW(HNSWIndexParams),
}

impl VectorIndexSpec {
    pub fn access_method(self) -> &'static str {
        match self {
            Self::BruteForce => "bruteforce",
            Self::IVF(_) => "ivf",
            Self::HNSW(_) => "hnsw",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorIndexOpenMode {
    Create,
    Restore,
}
