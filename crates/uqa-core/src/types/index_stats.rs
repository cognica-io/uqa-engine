//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index statistics shared by planners and scorers.

use super::FieldName;
use crate::memory::OwnedMap;
use keys::{TermIdentity, TermKey, TermRef, VectorKey};

mod keys;
mod owned;
#[cfg(test)]
mod tests;
mod vector;
pub use owned::{IndexStatsBuilder, OwnedIndexStats};
pub use vector::{
    DiskANNIndexStats, DiskANNQueryStats, VectorGeneration, VectorPopulationStats,
    VectorQueryRoute, VectorReadStats,
};

/// Index-level statistics consumed by the cost model and BM25 scorer.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub total_docs: u64,
    pub avg_doc_length: f64,
    pub dimensions: u32,
    doc_freqs: OwnedMap<TermKey, u64>,
    vector_queries: OwnedMap<VectorKey, DiskANNQueryStats>,
}

impl IndexStats {
    /// Build a new [`IndexStats`] with the given total document count
    /// and an empty frequency table.
    pub fn new(total_docs: u64) -> Self {
        Self {
            total_docs,
            avg_doc_length: 0.0,
            dimensions: 0,
            doc_freqs: OwnedMap::new(),
            vector_queries: OwnedMap::new(),
        }
    }

    /// Query coordinates retain their exact binary32 identity; distinct queries on one field can take different numeric routes.
    pub fn set_diskann_query(
        &mut self,
        field: impl Into<FieldName>,
        query: &[f32],
        stats: DiskANNQueryStats,
    ) {
        let field = field.into();
        if let Some(existing) = self
            .vector_queries
            .get_mut_by(|key| key.compare(&field, query))
        {
            *existing = stats;
        } else {
            self.vector_queries.insert(
                VectorKey {
                    field,
                    bits: query.iter().map(|value| value.to_bits()).collect(),
                },
                stats,
            );
        }
    }

    pub fn diskann_query(&self, field: &str, query: &[f32]) -> Option<&DiskANNQueryStats> {
        self.vector_queries.get_by(|key| key.compare(field, query))
    }

    pub fn doc_freq(&self, field: &str, term: &str) -> u64 {
        self.doc_freqs
            .get_by(|key| key.compare(field, TermRef::Scalar(term)))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_doc_freq(&mut self, field: impl Into<FieldName>, term: impl Into<String>, df: u64) {
        self.doc_freqs.insert(
            TermKey {
                field: field.into(),
                term: TermIdentity::Scalar(term.into()),
            },
            df,
        );
    }

    /// Exact UTF-16 lookup, including isolated surrogates. Scalar terms share identity with the string API.
    pub fn doc_freq_utf16(&self, field: &str, term: &[u16]) -> u64 {
        self.doc_freqs
            .get_by(|key| key.compare(field, TermRef::Units(term)))
            .copied()
            .unwrap_or(0)
    }

    /// Retain raw term identity without replacing unpaired units or creating a second representation for scalar text.
    pub fn set_doc_freq_utf16(&mut self, field: impl Into<FieldName>, term: Vec<u16>, df: u64) {
        self.doc_freqs.insert(
            TermKey {
                field: field.into(),
                term: TermIdentity::Units(term),
            },
            df,
        );
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
