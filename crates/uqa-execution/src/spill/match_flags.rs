//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable ordinal flags reuse authenticated temporary storage with one bounded page cache.
//!
//! The private cache is authoritative until replacement. No independent reader observes this file, so dropping the owner discards the last dirty page without a best-effort write that could hide an error.

use std::io::{Read, Seek, SeekFrom, Write};

use uqa_storage::temporary_file::TemporaryFile;

use crate::{ExecError, ExecResult};

const PAGE_BYTES: usize = 16 * 1024;

pub(crate) struct MatchFlags {
    file: TemporaryFile,
    rows: u64,
    capacity: u64,
    owner: &'static str,
    page: Box<[u8; PAGE_BYTES]>,
    page_start: Option<u64>,
    page_len: usize,
    dirty: bool,
    #[cfg(test)]
    page_reads: usize,
    #[cfg(test)]
    page_writes: usize,
}

impl MatchFlags {
    pub(crate) fn new(rows: u64, owner: &'static str) -> ExecResult<Self> {
        let file =
            TemporaryFile::new().map_err(|error| io_error(owner, "create match flags", error))?;
        file.set_len(rows)
            .map_err(|error| io_error(owner, "size match flags", error))?;
        Ok(Self {
            file,
            rows,
            capacity: rows,
            owner,
            page: Box::new([0; PAGE_BYTES]),
            page_start: None,
            page_len: 0,
            dirty: false,
            #[cfg(test)]
            page_reads: 0,
            #[cfg(test)]
            page_writes: 0,
        })
    }

    /// Append an unmatched ordinal without losing a dirty cached page. Capacity grows a page at a time while the visible index range remains exact.
    pub(crate) fn push_unmarked(&mut self) -> ExecResult<()> {
        let rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| ExecError::Other(format!("{} match flag count overflow", self.owner)))?;
        if rows > self.capacity {
            let capacity = rows
                .checked_add(PAGE_BYTES as u64 - 1)
                .map(|value| value / PAGE_BYTES as u64 * PAGE_BYTES as u64)
                .ok_or_else(|| {
                    ExecError::Other(format!("{} match flag capacity overflow", self.owner))
                })?;
            self.file
                .set_len(capacity)
                .map_err(|error| io_error(self.owner, "grow match flags", error))?;
            self.capacity = capacity;
        }
        if self
            .page_start
            .is_some_and(|start| self.rows >= start && self.rows - start < PAGE_BYTES as u64)
        {
            self.page_len = (rows - self.page_start.unwrap()) as usize;
        }
        self.rows = rows;
        Ok(())
    }

    pub(crate) fn mark(&mut self, index: u64) -> ExecResult<()> {
        let offset = self.load(index)?;
        if self.page[offset] == 0 {
            self.page[offset] = 1;
            self.dirty = true;
        }
        Ok(())
    }

    pub(crate) fn is_marked(&mut self, index: u64) -> ExecResult<bool> {
        let offset = self.load(index)?;
        Ok(self.page[offset] != 0)
    }

    pub(crate) fn check_index(&self, index: u64) -> ExecResult<()> {
        if index >= self.rows {
            return Err(ExecError::Other(format!(
                "{} match flag {index} is outside 0..{}",
                self.owner, self.rows
            )));
        }
        Ok(())
    }

    fn load(&mut self, index: u64) -> ExecResult<usize> {
        self.check_index(index)?;
        let start = index / PAGE_BYTES as u64 * PAGE_BYTES as u64;
        let offset = (index - start) as usize;
        if self.page_start == Some(start) {
            return Ok(offset);
        }
        self.flush()?;
        let len = (self.rows - start).min(PAGE_BYTES as u64) as usize;
        let mut page = [0; PAGE_BYTES];
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|error| io_error(self.owner, "seek match flags", error))?;
        self.file
            .read_exact(&mut page[..len])
            .map_err(|error| io_error(self.owner, "read match flag", error))?;
        #[cfg(test)]
        {
            self.page_reads += 1;
        }
        self.page.copy_from_slice(&page);
        self.page_start = Some(start);
        self.page_len = len;
        Ok(offset)
    }

    fn flush(&mut self) -> ExecResult<()> {
        if !self.dirty {
            return Ok(());
        }
        let start = self.page_start.expect("only a loaded page can be dirty");
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|error| io_error(self.owner, "seek match flags", error))?;
        self.file
            .write_all(&self.page[..self.page_len])
            .map_err(|error| io_error(self.owner, "write match flag", error))?;
        self.dirty = false;
        #[cfg(test)]
        {
            self.page_writes += 1;
        }
        Ok(())
    }
}

fn io_error(owner: &str, operation: &str, error: impl std::fmt::Display) -> ExecError {
    ExecError::Other(format!("{owner} spill {operation}: {error}"))
}

#[cfg(test)]
mod tests;
