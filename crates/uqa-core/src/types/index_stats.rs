//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index statistics shared by planners and scorers.

use super::{BTreeMap, FieldName};

/// Index-level statistics consumed by the cost model and BM25 scorer.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub total_docs: u64,
    pub avg_doc_length: f64,
    pub dimensions: u32,
    doc_freqs: BTreeMap<(FieldName, String), u64>,
}

impl IndexStats {
    /// Build a new [`IndexStats`] with the given total document count
    /// and an empty frequency table.
    pub fn new(total_docs: u64) -> Self {
        Self {
            total_docs,
            avg_doc_length: 0.0,
            dimensions: 0,
            doc_freqs: BTreeMap::new(),
        }
    }

    pub fn doc_freq(&self, field: &str, term: &str) -> u64 {
        self.doc_freqs
            .get(&(field.to_string(), term.to_string()))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_doc_freq(&mut self, field: impl Into<FieldName>, term: impl Into<String>, df: u64) {
        self.doc_freqs.insert((field.into(), term.into()), df);
    }

    /// Builder-style insert that returns the modified [`IndexStats`].
    pub fn with_doc_freq(
        mut self,
        field: impl Into<FieldName>,
        term: impl Into<String>,
        df: u64,
    ) -> Self {
        self.set_doc_freq(field, term, df);
        self
    }
}
