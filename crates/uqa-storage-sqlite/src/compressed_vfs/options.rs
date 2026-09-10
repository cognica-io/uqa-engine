//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compression codec selection and validated chunk geometry.

use super::{DEFAULT_CHUNK_PAGES, DEFAULT_LEVEL, DEFAULT_PAGE_SIZE};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SQLiteCompressionCodec {
    #[default]
    Zstd,
    LZ4,
}

impl SQLiteCompressionCodec {
    pub(super) const fn id(self) -> u32 {
        match self {
            Self::Zstd => 1,
            Self::LZ4 => 2,
        }
    }

    pub(super) fn from_id(id: u32) -> Result<Self, String> {
        match id {
            0 | 1 => Ok(Self::Zstd),
            2 => Ok(Self::LZ4),
            _ => Err(format!("unsupported compression codec id {id}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SQLiteCompressionOptions {
    pub codec: SQLiteCompressionCodec,
    pub page_size: u32,
    pub chunk_pages: u32,
    pub level: i32,
}

impl Default for SQLiteCompressionOptions {
    fn default() -> Self {
        Self {
            codec: SQLiteCompressionCodec::default(),
            page_size: DEFAULT_PAGE_SIZE,
            chunk_pages: DEFAULT_CHUNK_PAGES,
            level: DEFAULT_LEVEL,
        }
    }
}

impl SQLiteCompressionOptions {
    pub fn zstd() -> Self {
        Self {
            codec: SQLiteCompressionCodec::Zstd,
            ..Self::default()
        }
    }

    pub fn lz4() -> Self {
        Self {
            codec: SQLiteCompressionCodec::LZ4,
            level: 0,
            ..Self::default()
        }
    }

    pub fn validate(self) -> Result<Self, String> {
        if self.page_size == 0 || !self.page_size.is_power_of_two() {
            return Err("page_size must be a non-zero power of two".to_string());
        }
        if !(512..=65_536).contains(&self.page_size) {
            return Err("page_size must be between 512 and 65536 bytes".to_string());
        }
        if self.chunk_pages == 0 {
            return Err("chunk_pages must be non-zero".to_string());
        }
        let chunk_size = u64::from(self.page_size) * u64::from(self.chunk_pages);
        if !(u64::from(self.page_size)..=1_048_576).contains(&chunk_size) {
            return Err("chunk size must be at most 1 MiB".to_string());
        }
        if self.codec == SQLiteCompressionCodec::Zstd && !(-7..=22).contains(&self.level) {
            return Err("zstd level must be between -7 and 22".to_string());
        }
        Ok(self)
    }

    pub fn chunk_size(self) -> Result<usize, String> {
        let validated = self.validate()?;
        let page_size = usize::try_from(validated.page_size)
            .map_err(|_| "page_size exceeds the addressable range".to_string())?;
        let chunk_pages = usize::try_from(validated.chunk_pages)
            .map_err(|_| "chunk_pages exceeds the addressable range".to_string())?;
        page_size
            .checked_mul(chunk_pages)
            .ok_or_else(|| "chunk size exceeds the addressable range".to_string())
    }
}
