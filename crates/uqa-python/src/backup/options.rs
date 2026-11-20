//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::compression_options;
use pyo3::{exceptions::PyValueError, prelude::*};
use std::path::PathBuf;
use uqa_engine::backup::{
    BackupOptions, RestoreDestination, RestoreOptions, SQLiteBackupMapping, SQLiteBackupTarget,
    StorageEncryptionKey,
};

#[pyclass(
    name = "BackupOptions",
    module = "uqa._uqa",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Copy, Default)]
pub(super) struct PyBackupOptions {
    pub(super) capture: BackupOptions,
    pub(super) memory_bytes: Option<usize>,
}

#[pymethods]
impl PyBackupOptions {
    #[new]
    #[pyo3(signature = (*, maximum_encoded_bytes=None, maximum_workspace_bytes=None, memory_bytes=None, allow_plaintext_export=false))]
    fn new(
        maximum_encoded_bytes: Option<u64>,
        maximum_workspace_bytes: Option<u64>,
        memory_bytes: Option<usize>,
        allow_plaintext_export: bool,
    ) -> Self {
        let defaults = BackupOptions::default();
        Self {
            capture: BackupOptions {
                maximum_encoded_bytes: maximum_encoded_bytes
                    .unwrap_or(defaults.maximum_encoded_bytes),
                maximum_workspace_bytes: maximum_workspace_bytes
                    .unwrap_or(defaults.maximum_workspace_bytes),
                allow_plaintext_export,
                ..defaults
            },
            memory_bytes,
        }
    }
}

#[pyclass(
    name = "RestoreOptions",
    module = "uqa._uqa",
    frozen,
    skip_from_py_object
)]
#[derive(Clone, Copy, Default)]
pub(super) struct PyRestoreOptions {
    pub(super) restore: RestoreOptions,
    pub(super) memory_bytes: Option<usize>,
}

#[pymethods]
impl PyRestoreOptions {
    #[new]
    #[pyo3(signature = (*, maximum_workspace_bytes=None, maximum_value_bytes=None, retained_bytes=None, memory_bytes=None))]
    fn new(
        maximum_workspace_bytes: Option<u64>,
        maximum_value_bytes: Option<usize>,
        retained_bytes: Option<usize>,
        memory_bytes: Option<usize>,
    ) -> Self {
        let defaults = RestoreOptions::default();
        Self {
            restore: RestoreOptions {
                maximum_workspace_bytes: maximum_workspace_bytes
                    .unwrap_or(defaults.maximum_workspace_bytes),
                maximum_value_bytes: maximum_value_bytes.unwrap_or(defaults.maximum_value_bytes),
                retained_bytes: retained_bytes.unwrap_or(defaults.retained_bytes),
            },
            memory_bytes,
        }
    }
}

#[pyclass(name = "RestoreDestination", module = "uqa._uqa", frozen)]
pub(super) struct PyRestoreDestination(pub(super) RestoreDestination);

#[pymethods]
impl PyRestoreDestination {
    #[staticmethod]
    fn memory() -> Self {
        Self(RestoreDestination::Memory)
    }

    /// Install a native or key/value database in a new destination directory.
    #[staticmethod]
    #[pyo3(signature = (directory, *, mapping="native", key=None, codec=None, page_size=None, chunk_pages=None, level=None))]
    fn sqlite(
        directory: PathBuf,
        mapping: &str,
        key: Option<&str>,
        codec: Option<&str>,
        page_size: Option<u32>,
        chunk_pages: Option<u32>,
        level: Option<i32>,
    ) -> PyResult<Self> {
        let mapping = match mapping {
            "native" => SQLiteBackupMapping::Native,
            "kv" => SQLiteBackupMapping::KeyValue,
            _ => {
                return Err(PyValueError::new_err(
                    "SQLite mapping must be 'native' or 'kv'",
                ))
            }
        };
        let mut target = SQLiteBackupTarget::new(mapping);
        target.encryption_key = key.map(StorageEncryptionKey::new);
        target.compression = match codec {
            Some(codec) => Some(compression_options(codec, page_size, chunk_pages, level)?),
            None if page_size.is_some() || chunk_pages.is_some() || level.is_some() => {
                return Err(PyValueError::new_err(
                    "SQLite compression options require a codec",
                ));
            }
            None => None,
        };
        Ok(Self(RestoreDestination::persistent(directory, target)))
    }
}
