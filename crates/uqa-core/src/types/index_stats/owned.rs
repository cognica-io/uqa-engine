//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native statistics retain exact constructor reservations until the complete immutable output is released.

use super::{
    keys::{TermIdentity, TermKey, TermRef, VectorKey},
    DiskANNQueryStats, IndexStats,
};
use crate::{
    memory::{MemoryReservation, OwnedMap, ProductionControl, ProductionVec},
    ValueRetentionError,
};

#[derive(Debug)]
pub struct OwnedIndexStats {
    stats: IndexStats,
    memory: Option<MemoryReservation>,
}

impl OwnedIndexStats {
    pub fn reserved_bytes(&self) -> usize {
        self.memory.as_ref().map_or(0, MemoryReservation::bytes)
    }

    /// Change only the inline population estimate, preserving every retained allocation and its lease.
    pub fn with_total_docs(mut self, total_docs: u64) -> Self {
        self.stats.total_docs = total_docs;
        self
    }
}

impl From<IndexStats> for OwnedIndexStats {
    fn from(stats: IndexStats) -> Self {
        Self {
            stats,
            memory: None,
        }
    }
}

impl std::ops::Deref for OwnedIndexStats {
    type Target = IndexStats;

    fn deref(&self) -> &IndexStats {
        &self.stats
    }
}

/// The only mutable admitted statistics owner. Replacements change allocation-free values in place; new keys and complete map nodes reserve before construction. Publication exposes no mutable access to the retained maps.
pub struct IndexStatsBuilder<'a> {
    stats: IndexStats,
    memory: Option<MemoryReservation>,
    control: ProductionControl<'a>,
}

impl<'a> IndexStatsBuilder<'a> {
    pub fn new(total_docs: u64, control: ProductionControl<'a>) -> Self {
        Self {
            stats: IndexStats::new(total_docs),
            memory: control.empty_reservation(),
            control,
        }
    }

    pub fn include_vector_dimensions(&mut self, dimensions: u32) {
        self.stats.dimensions = self.stats.dimensions.max(dimensions);
    }

    pub fn set_doc_freq(
        &mut self,
        field: &str,
        term: &str,
        df: u64,
    ) -> Result<(), ValueRetentionError> {
        self.insert_frequency(field, TermRef::Scalar(term), df)
    }

    pub fn set_doc_freq_utf16(
        &mut self,
        field: &str,
        term: &[u16],
        df: u64,
    ) -> Result<(), ValueRetentionError> {
        self.insert_frequency(field, TermRef::Units(term), df)
    }

    fn insert_frequency(
        &mut self,
        field: &str,
        term: TermRef<'_>,
        df: u64,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        if let Some(value) = self
            .stats
            .doc_freqs
            .get_mut_by(|key| key.compare(field, term))
        {
            *value = df;
            return Ok(());
        }
        let field = self.control.copy_text(field)?;
        let (term, memory) = match term {
            TermRef::Scalar(text) => {
                let (text, memory) = self.control.copy_text(text)?.into_parts();
                (TermIdentity::Scalar(text), memory)
            }
            TermRef::Units(units) => {
                let mut output = ProductionVec::new(self.control);
                output.extend_from_slice(units)?;
                let (units, memory) = output.finish()?.into_parts();
                (TermIdentity::Units(units), memory)
            }
        };
        let (field, field_memory) = field.into_parts();
        let key = self.control.finish(
            TermKey { field, term },
            self.control.combine(field_memory, memory),
        )?;
        let node = self
            .control
            .reserve(OwnedMap::<TermKey, u64>::entry_bytes())?;
        let (key, key_memory) = key.into_parts();
        self.stats.doc_freqs.insert(key, df);
        self.retain(self.control.combine(key_memory, node));
        Ok(())
    }

    pub fn set_diskann_query(
        &mut self,
        field: &str,
        query: &[f32],
        stats: DiskANNQueryStats,
    ) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        if let Some(value) = self
            .stats
            .vector_queries
            .get_mut_by(|key| key.compare(field, query))
        {
            *value = stats;
            return Ok(());
        }
        let field = self.control.copy_text(field)?;
        let mut bits = ProductionVec::new(self.control);
        bits.reserve(query.len())?;
        for value in query {
            bits.push_copy(value.to_bits())?;
        }
        let (bits, bits_memory) = bits.finish()?.into_parts();
        let (field, field_memory) = field.into_parts();
        let key = self.control.finish(
            VectorKey { field, bits },
            self.control.combine(field_memory, bits_memory),
        )?;
        let node = self
            .control
            .reserve(OwnedMap::<VectorKey, DiskANNQueryStats>::entry_bytes())?;
        let (key, key_memory) = key.into_parts();
        self.stats.vector_queries.insert(key, stats);
        self.retain(self.control.combine(key_memory, node));
        Ok(())
    }

    fn retain(&mut self, memory: Option<MemoryReservation>) {
        self.memory = self.control.combine(self.memory.take(), memory);
    }

    pub fn finish(self) -> Result<OwnedIndexStats, ValueRetentionError> {
        let output = OwnedIndexStats {
            stats: self.stats,
            memory: self.memory,
        };
        self.control.check()?;
        Ok(output)
    }
}

impl std::ops::Deref for IndexStatsBuilder<'_> {
    type Target = IndexStats;

    fn deref(&self) -> &IndexStats {
        &self.stats
    }
}
