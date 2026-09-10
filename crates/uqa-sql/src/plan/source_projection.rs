//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required SQL source columns and explicitly requested relation metadata.

use crate::ScalarExpr;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelationMetadataProjection(u8);

impl RelationMetadataProjection {
    const DOC_ID: u8 = 1;
    const SCORE: u8 = 2;

    pub fn request_doc_id(&mut self) {
        self.0 |= Self::DOC_ID;
    }

    pub fn request_score(&mut self) {
        self.0 |= Self::SCORE;
    }

    pub fn includes_doc_id(self) -> bool {
        self.0 & Self::DOC_ID != 0
    }

    pub fn includes_score(self) -> bool {
        self.0 & Self::SCORE != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceProjection {
    columns: BTreeSet<String>,
    retain_all: bool,
    metadata: RelationMetadataProjection,
}

impl SourceProjection {
    pub fn retaining_all() -> Self {
        Self {
            retain_all: true,
            ..Self::default()
        }
    }

    pub fn contains(&self, column: &str) -> bool {
        self.retain_all || self.columns.contains(column)
    }

    pub fn retain_all(&mut self) {
        self.retain_all = true;
    }

    pub fn insert(&mut self, column: String) {
        self.columns.insert(column);
    }

    pub fn extend(&mut self, columns: impl IntoIterator<Item = String>) {
        self.columns.extend(columns);
    }

    pub fn explicit_columns(self) -> Option<BTreeSet<String>> {
        (!self.retain_all).then_some(self.columns)
    }

    pub fn metadata(&self) -> RelationMetadataProjection {
        self.metadata
    }

    pub fn metadata_mut(&mut self) -> &mut RelationMetadataProjection {
        &mut self.metadata
    }
}

pub type ColumnPrune = BTreeMap<String, SourceProjection>;
pub type QualifierFilters = BTreeMap<String, Vec<ScalarExpr>>;
