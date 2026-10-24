//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy native `SQLite` anchor fields without interpreting or serializing their authentication data.

use std::{ffi::c_char, path::Path};
use uqa_storage_sqlite::SQLiteCompressedContainerAnchor;

use super::UQAConfig;
use crate::{
    abi::{UQAStatus, UQA_DONE, UQA_ERROR_IO, UQA_OK},
    boundary,
    error::UQAError,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UQASQLiteContainerAnchor {
    pub database_id: [u8; 16],
    pub generation: u64,
    pub state_tag: [u8; 32],
}

impl From<SQLiteCompressedContainerAnchor> for UQASQLiteContainerAnchor {
    fn from(value: SQLiteCompressedContainerAnchor) -> Self {
        Self {
            database_id: value.database_id,
            generation: value.generation,
            state_tag: value.state_tag,
        }
    }
}

impl From<UQASQLiteContainerAnchor> for SQLiteCompressedContainerAnchor {
    fn from(value: UQASQLiteContainerAnchor) -> Self {
        Self {
            database_id: value.database_id,
            generation: value.generation,
            state_tag: value.state_tag,
        }
    }
}

ffi_fn!(uqa_config_get_trusted_anchor(config: *mut UQAConfig, out: *mut UQASQLiteContainerAnchor) {
    let out = boundary::output(out)?;
    match boundary::input(config)?.trusted_anchor {
        Some(anchor) => { *out = anchor.into(); Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});

ffi_fn!(uqa_config_set_trusted_anchor(config: *mut UQAConfig, anchor: *const UQASQLiteContainerAnchor) {
    let value = if anchor.is_null() { None } else { Some((*boundary::input(anchor)?).into()) };
    boundary::input_mut(config)?.trusted_anchor = value;
    Ok(UQA_OK)
});

fn read(path: &str, key: &str, out: &mut UQASQLiteContainerAnchor) -> boundary::Result<UQAStatus> {
    if path.is_empty() || key.is_empty() {
        return Err(UQAError::invalid(
            "container path and encryption key must not be empty",
        ));
    }
    *out = uqa_storage_sqlite::read_authenticated_anchor(Path::new(path), key)
        .map_err(|error| UQAError::display(UQA_ERROR_IO, &error))?
        .into();
    Ok(UQA_OK)
}

ffi_fn!(uqa_sqlite_get_container_anchor(path: *const c_char, key: *const c_char, out: *mut UQASQLiteContainerAnchor) {
    let out = boundary::output(out)?;
    read(boundary::text(path)?, boundary::text(key)?, out)
});

ffi_fn!(uqa_sqlite_get_container_anchor_n(path: *const c_char, path_size: usize, key: *const c_char, key_size: usize, out: *mut UQASQLiteContainerAnchor) {
    let out = boundary::output(out)?;
    read(boundary::text_n(path, path_size)?, boundary::text_n(key, key_size)?, out)
});
