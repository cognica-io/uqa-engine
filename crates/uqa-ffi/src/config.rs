//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned configuration; setters validate and copy before replacing existing storage.

use std::ffi::c_char;
use uqa_storage_sqlite::{
    SQLiteCompressedContainerAnchor, SQLiteCompressionCodec, SQLiteCompressionOptions,
};
use zeroize::Zeroizing;

mod anchor;
pub use anchor::*;

use crate::{
    abi::{
        UQAStatus, UQAStringView, UQA_COMPRESSION_LZ4, UQA_COMPRESSION_NONE, UQA_COMPRESSION_ZSTD,
        UQA_DONE, UQA_ERROR_INVALID_ARGUMENT, UQA_ERROR_RANGE, UQA_OK,
    },
    boundary,
    error::UQAError,
};

pub struct UQAConfig {
    pub(crate) key: Option<Zeroizing<String>>,
    pub(crate) compression: Option<SQLiteCompressionOptions>,
    pub(crate) trusted_anchor: Option<SQLiteCompressedContainerAnchor>,
    pub(crate) memory_limit: usize,
}

impl Default for UQAConfig {
    fn default() -> Self {
        Self {
            key: None,
            compression: None,
            trusted_anchor: None,
            memory_limit: 64 * 1024 * 1024,
        }
    }
}

impl UQAConfig {
    fn set_key(&mut self, key: Zeroizing<String>) -> boundary::Result<UQAStatus> {
        if key.is_empty() {
            return Err(UQAError::invalid("encryption key must not be empty"));
        }
        self.key = Some(key);
        Ok(UQA_OK)
    }
}

ffi_fn!(uqa_config_create(out: *mut *mut UQAConfig) {
    let out = boundary::new_handle(out)?;
    *out = Box::into_raw(boundary::boxed(UQAConfig::default())?);
    Ok(UQA_OK)
});
ffi_fn!(uqa_config_destroy(config: *mut *mut UQAConfig) { boundary::release(config) });
ffi_fn!(uqa_config_get_encryption_key(config: *mut UQAConfig, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    match boundary::input(config)?.key.as_ref() {
        Some(key) => { *out = key.as_str().into(); Ok(UQA_OK) }
        None => Ok(UQA_DONE),
    }
});
ffi_fn!(uqa_config_set_encryption_key(config: *mut UQAConfig, key: *const c_char) {
    // Copy before obtaining a mutable reference: key may borrow the current config.
    let key = Zeroizing::new(boundary::copy_text(boundary::text(key)?)?);
    boundary::input_mut(config)?.set_key(key)
});
ffi_fn!(uqa_config_set_encryption_key_n(config: *mut UQAConfig, key: *const c_char, key_size: usize) {
    let key = Zeroizing::new(boundary::copy_text(boundary::text_n(key, key_size)?)?);
    boundary::input_mut(config)?.set_key(key)
});
ffi_fn!(uqa_config_get_compression(config: *mut UQAConfig, codec: *mut u32, level: *mut i32) {
    let codec = boundary::output(codec)?;
    let level = boundary::output(level)?;
    if let Some(options) = boundary::input(config)?.compression {
        *codec = match options.codec { SQLiteCompressionCodec::Zstd => UQA_COMPRESSION_ZSTD, SQLiteCompressionCodec::LZ4 => UQA_COMPRESSION_LZ4 };
        *level = options.level;
    }
    Ok(UQA_OK)
});
ffi_fn!(uqa_config_set_compression(config: *mut UQAConfig, codec: u32, level: i32) {
    let value = match codec {
        UQA_COMPRESSION_NONE if level == 0 => None,
        UQA_COMPRESSION_ZSTD | UQA_COMPRESSION_LZ4 => {
            let codec = if codec == UQA_COMPRESSION_ZSTD { SQLiteCompressionCodec::Zstd } else { SQLiteCompressionCodec::LZ4 };
            if codec == SQLiteCompressionCodec::LZ4 && level != 0 { return Err(UQAError::invalid("LZ4 compression has no level setting")); }
            Some(SQLiteCompressionOptions { codec, level, ..SQLiteCompressionOptions::default() }.validate()
                .map_err(|message| UQAError::new(UQA_ERROR_INVALID_ARGUMENT, message))?)
        }
        _ => return Err(UQAError::invalid("invalid compression codec or level")),
    };
    boundary::input_mut(config)?.compression = value;
    Ok(UQA_OK)
});
ffi_fn!(uqa_config_get_ffi_memory_limit(config: *mut UQAConfig, bytes: *mut u64) {
    let bytes = boundary::output(bytes)?;
    *bytes = boundary::input(config)?.memory_limit as u64;
    Ok(UQA_OK)
});
ffi_fn!(uqa_config_set_ffi_memory_limit(config: *mut UQAConfig, bytes: u64) {
    let bytes = usize::try_from(bytes).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "memory limit exceeds addressable range"))?;
    if bytes == 0 || bytes > isize::MAX as usize { return Err(UQAError::invalid("memory limit must be positive and addressable")); }
    boundary::input_mut(config)?.memory_limit = bytes;
    Ok(UQA_OK)
});
