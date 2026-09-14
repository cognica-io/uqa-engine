//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// Bounds for input bytes, decompressed section bytes, and decoded string metadata.
#[derive(Debug, Clone, Copy)]
pub struct DictionaryLimits {
    pub max_encoded_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_manifest_bytes: usize,
    pub max_text_utf16: usize,
    pub max_strings: usize,
}

impl Default for DictionaryLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 128 * 1024 * 1024,
            max_decoded_bytes: 256 * 1024 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_text_utf16: u16::MAX as usize,
            max_strings: 1_000_000,
        }
    }
}
