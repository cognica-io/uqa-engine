//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional view-event mapping: contiguous ordinals stay implicit; fragmented ordinals use one authenticated page per lookup locality.

use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;

use uqa_core::CancellationToken;
use uqa_sql::SQLError;
use uqa_storage::temporary_file::TemporaryFile;

use super::{invalid, IndexStorage, RuleIndices};

const WORD_BYTES: usize = 8;
const PAGE_BYTES: usize = 16 * 1024;
const PAGE_WORDS: usize = PAGE_BYTES / WORD_BYTES;

pub(crate) struct RuleIndexLookup {
    storage: LookupStorage,
    count: usize,
    cancellation: CancellationToken,
}

enum LookupStorage {
    Range(Range<usize>),
    Pages(Box<OrdinalPages>),
}

impl RuleIndices {
    pub(crate) fn lookup(&self) -> Result<RuleIndexLookup, SQLError> {
        self.cancellation.check()?;
        let storage = match &self.storage {
            IndexStorage::Range(range) => {
                if range.len() != self.count {
                    return Err(invalid("positional selection count changed"));
                }
                LookupStorage::Range(range.clone())
            }
            IndexStorage::Spill(_) => LookupStorage::Pages(Box::new(OrdinalPages::build(self)?)),
        };
        Ok(RuleIndexLookup {
            storage,
            count: self.count,
            cancellation: self.cancellation.clone(),
        })
    }
}

impl RuleIndexLookup {
    pub(crate) fn get(&mut self, index: usize) -> Result<usize, SQLError> {
        self.cancellation.check()?;
        if index >= self.count {
            return Err(invalid("positional index is outside the event selection"));
        }
        match &mut self.storage {
            LookupStorage::Range(range) => Ok(range.start + index),
            LookupStorage::Pages(pages) => pages.get(index),
        }
    }
}

struct OrdinalPages {
    file: TemporaryFile,
    byte_len: u64,
    page: Box<[u8; PAGE_BYTES]>,
    page_start: Option<u64>,
    #[cfg(test)]
    page_reads: usize,
    #[cfg(test)]
    page_writes: usize,
}

impl OrdinalPages {
    fn build(indices: &RuleIndices) -> Result<Self, SQLError> {
        let byte_len = (indices.count as u64)
            .checked_mul(WORD_BYTES as u64)
            .ok_or_else(|| invalid("positional selection exceeds the spill address space"))?;
        let mut pages = Self {
            file: TemporaryFile::new().map_err(io_error)?,
            byte_len,
            page: Box::new([0; PAGE_BYTES]),
            page_start: None,
            #[cfg(test)]
            page_reads: 0,
            #[cfg(test)]
            page_writes: 0,
        };
        let mut used = 0;
        for ordinal in indices.reader()? {
            let value = ordinal? as u64;
            pages.page[used..used + WORD_BYTES].copy_from_slice(&value.to_le_bytes());
            used += WORD_BYTES;
            if used == PAGE_BYTES {
                pages.write_page(used)?;
                used = 0;
            }
        }
        if used != 0 {
            pages.write_page(used)?;
        }
        Ok(pages)
    }

    fn write_page(&mut self, len: usize) -> Result<(), SQLError> {
        self.file.write_all(&self.page[..len]).map_err(io_error)?;
        #[cfg(test)]
        {
            self.page_writes += 1;
        }
        Ok(())
    }

    fn get(&mut self, index: usize) -> Result<usize, SQLError> {
        let start = (index / PAGE_WORDS) as u64 * PAGE_BYTES as u64;
        if self.page_start != Some(start) {
            // A failed read cannot leave a partially overwritten page marked valid.
            self.page_start = None;
            let len = (self.byte_len - start).min(PAGE_BYTES as u64) as usize;
            self.file.seek(SeekFrom::Start(start)).map_err(io_error)?;
            self.file
                .read_exact(&mut self.page[..len])
                .map_err(io_error)?;
            self.page_start = Some(start);
            #[cfg(test)]
            {
                self.page_reads += 1;
            }
        }
        let offset = index % PAGE_WORDS * WORD_BYTES;
        let value = u64::from_le_bytes(
            self.page[offset..offset + WORD_BYTES]
                .try_into()
                .expect("ordinal words have a fixed width"),
        );
        usize::try_from(value).map_err(|_| invalid("ordinal exceeds the addressable row count"))
    }
}

fn io_error(error: std::io::Error) -> SQLError {
    invalid(&format!("positional spill: {error}"))
}

#[cfg(test)]
mod tests;
