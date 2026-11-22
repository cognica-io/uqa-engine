//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded cache of decompressed chunks for one compressed-container handle.

use super::{invalid_data, BTreeMap};

/// Decompressed chunk contents retained by one container handle.
///
/// A dirty chunk holds changes that exist nowhere else until the container
/// publishes them, so it is never evicted. A clean chunk is a copy of a
/// committed record. Clean chunks are evicted in least-recently-used order
/// once their total size exceeds the clean budget, so a handle retains at most
/// that budget of clean contents however much of the file it reads.
#[derive(Debug)]
pub(super) struct ChunkCache {
    chunks: BTreeMap<u64, CachedChunk>,
    clean_by_use: BTreeMap<u64, u64>,
    clean_budget: usize,
    clean_bytes: usize,
    dirty_bytes: usize,
    dirty_count: usize,
    next_use: u64,
}

#[derive(Debug)]
struct CachedChunk {
    bytes: Vec<u8>,
    last_use: Option<u64>,
}

impl ChunkCache {
    pub(super) fn new(clean_budget: usize) -> Self {
        Self {
            chunks: BTreeMap::new(),
            clean_by_use: BTreeMap::new(),
            clean_budget,
            clean_bytes: 0,
            dirty_bytes: 0,
            dirty_count: 0,
            next_use: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.chunks.len()
    }

    pub(super) fn contains(&self, chunk_id: u64) -> bool {
        self.chunks.contains_key(&chunk_id)
    }

    #[cfg(test)]
    pub(super) fn clean_bytes(&self) -> usize {
        self.clean_bytes
    }

    pub(super) fn dirty_bytes(&self) -> usize {
        self.dirty_bytes
    }

    pub(super) fn has_dirty(&self) -> bool {
        self.dirty_count > 0
    }

    pub(super) fn clean_budget(&self) -> usize {
        self.clean_budget
    }

    pub(super) fn set_clean_budget(&mut self, clean_budget: usize) {
        self.clean_budget = clean_budget;
        self.evict_clean_over_budget(0);
    }

    /// Identifiers of the dirty chunks in ascending order.
    pub(super) fn dirty_ids(&self) -> Vec<u64> {
        self.chunks
            .iter()
            .filter(|(_, chunk)| chunk.last_use.is_none())
            .map(|(chunk_id, _)| *chunk_id)
            .collect()
    }

    /// Borrow a cached chunk without changing its recency.
    pub(super) fn peek(&self, chunk_id: u64) -> Option<&[u8]> {
        self.chunks
            .get(&chunk_id)
            .map(|chunk| chunk.bytes.as_slice())
    }

    /// Borrow a cached chunk sized to `len`, marking a clean chunk as the most
    /// recently used.
    pub(super) fn get_mut(&mut self, chunk_id: u64, len: usize) -> Option<&mut [u8]> {
        let chunk = self.chunks.get_mut(&chunk_id)?;
        let previous_len = chunk.bytes.len();
        chunk.bytes.resize(len, 0);
        match chunk.last_use {
            Some(last_use) => {
                let use_tick = self.next_use;
                self.next_use += 1;
                self.clean_by_use.remove(&last_use);
                self.clean_by_use.insert(use_tick, chunk_id);
                chunk.last_use = Some(use_tick);
                self.clean_bytes = self.clean_bytes - previous_len + len;
            }
            None => self.dirty_bytes = self.dirty_bytes - previous_len + len,
        }
        Some(chunk.bytes.as_mut_slice())
    }

    /// Retain a chunk decoded from its committed record. Least recently used
    /// clean chunks are evicted first so that the new chunk fits the budget.
    pub(super) fn insert_clean(&mut self, chunk_id: u64, bytes: Vec<u8>) -> &mut [u8] {
        if let Some(previous) = self.chunks.remove(&chunk_id) {
            self.forget(&previous);
        }
        self.evict_clean_over_budget(bytes.len());
        let use_tick = self.next_use;
        self.next_use += 1;
        self.clean_bytes += bytes.len();
        self.clean_by_use.insert(use_tick, chunk_id);
        let chunk = self.chunks.entry(chunk_id).or_insert(CachedChunk {
            bytes,
            last_use: Some(use_tick),
        });
        chunk.bytes.as_mut_slice()
    }

    /// Mark a cached chunk as changed by this handle.
    pub(super) fn mark_dirty(&mut self, chunk_id: u64) -> std::io::Result<()> {
        let chunk = self.chunks.get_mut(&chunk_id).ok_or_else(|| {
            invalid_data(format!("chunk {chunk_id} changed without being cached"))
        })?;
        if let Some(last_use) = chunk.last_use.take() {
            self.clean_by_use.remove(&last_use);
            self.clean_bytes -= chunk.bytes.len();
            self.dirty_bytes += chunk.bytes.len();
            self.dirty_count += 1;
        }
        Ok(())
    }

    /// Turn every dirty chunk into a clean copy of the record the container
    /// has just published, then evict clean chunks beyond the budget.
    pub(super) fn publish_dirty(&mut self) {
        for (chunk_id, chunk) in &mut self.chunks {
            if chunk.last_use.is_none() {
                chunk.last_use = Some(self.next_use);
                self.clean_by_use.insert(self.next_use, *chunk_id);
                self.next_use += 1;
                self.clean_bytes += chunk.bytes.len();
            }
        }
        self.dirty_bytes = 0;
        self.dirty_count = 0;
        self.evict_clean_over_budget(0);
    }

    /// Discard every chunk. The caller has already published or rejected all
    /// dirty chunks.
    pub(super) fn clear(&mut self) {
        self.chunks.clear();
        self.clean_by_use.clear();
        self.clean_bytes = 0;
        self.dirty_bytes = 0;
        self.dirty_count = 0;
    }

    /// Discard one chunk whose committed record another handle replaced.
    pub(super) fn remove(&mut self, chunk_id: u64) {
        if let Some(chunk) = self.chunks.remove(&chunk_id) {
            self.forget(&chunk);
        }
    }

    /// Remove the chunks at or beyond `chunk_count`.
    pub(super) fn retain_below(&mut self, chunk_count: u64) {
        let removed = self.chunks.split_off(&chunk_count);
        for chunk in removed.values() {
            self.forget(chunk);
        }
    }

    fn forget(&mut self, chunk: &CachedChunk) {
        if let Some(last_use) = chunk.last_use {
            self.clean_by_use.remove(&last_use);
            self.clean_bytes -= chunk.bytes.len();
        } else {
            self.dirty_bytes -= chunk.bytes.len();
            self.dirty_count -= 1;
        }
    }

    fn evict_clean_over_budget(&mut self, incoming: usize) {
        while self.clean_bytes + incoming > self.clean_budget {
            let Some((_, chunk_id)) = self.clean_by_use.pop_first() else {
                return;
            };
            if let Some(chunk) = self.chunks.remove(&chunk_id) {
                self.clean_bytes -= chunk.bytes.len();
            }
        }
    }
}
