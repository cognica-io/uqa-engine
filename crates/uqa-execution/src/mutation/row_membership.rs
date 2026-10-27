//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact physical row membership for mutation deduplication and deferred referential checks.

use uqa_core::{memory::MemoryBudget, CancellationToken, DocId};
use uqa_sql::SQLError;
use uqa_storage::{
    mvcc::{
        PrivateRecordChanges, PrivateRecordReadScope, PrivateRecordSnapshot, RecordWrite,
        VersionError,
    },
    read_control::StorageReadControl,
};

pub(crate) type PhysicalRowLookup<'a> = dyn Fn(&str, DocId) -> Result<bool, SQLError> + 'a;

/// Ordered spill needs fixed run metadata and I/O scratch even when `work_mem` forces every key to disk.
const SPILL_WORKSPACE_BYTES: usize = 64 * 1024;

/// Accumulate exact physical keys in the storage owner's bounded ordered runs. Callers may freeze a completed selection for subsequent membership probes.
pub(crate) struct PhysicalRowBuffer {
    records: PrivateRecordChanges,
    control: StorageReadControl,
    operation: &'static str,
}

impl PhysicalRowBuffer {
    pub(crate) fn new(
        bytes: usize,
        cancellation: CancellationToken,
        operation: &'static str,
    ) -> Self {
        let memory = MemoryBudget::new(bytes.max(SPILL_WORKSPACE_BYTES));
        let records = if bytes < SPILL_WORKSPACE_BYTES {
            PrivateRecordChanges::with_memory_tier_limit(&memory, bytes)
        } else {
            PrivateRecordChanges::new(&memory)
        };
        Self {
            records,
            control: StorageReadControl::new(&memory, &cancellation),
            operation,
        }
    }

    pub(crate) fn insert(&mut self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        self.control.cancellation().check()?;
        let key = physical_key(table, doc_id);
        self.write_key(&key)
    }

    /// A tuple recheck may resolve several candidate identities to the same current row; only its first qualification proceeds.
    pub(crate) fn insert_if_absent(
        &mut self,
        table: &str,
        doc_id: DocId,
    ) -> Result<bool, SQLError> {
        self.control.cancellation().check()?;
        let key = physical_key(table, doc_id);
        if self.contains_key(&key)? {
            return Ok(false);
        }
        self.write_key(&key)?;
        Ok(true)
    }

    pub(crate) fn contains(&self, table: &str, doc_id: DocId) -> Result<bool, SQLError> {
        self.control.cancellation().check()?;
        self.contains_key(&physical_key(table, doc_id))
    }

    fn contains_key(&self, key: &[u8]) -> Result<bool, SQLError> {
        self.records
            .snapshot()
            .and_then(|view| view.get(key, &self.control))
            .map(|record| record.is_some())
            .map_err(|error| storage_error(self.operation, error))
    }

    fn write_key(&self, key: &[u8]) -> Result<(), SQLError> {
        self.records
            .apply(
                &[RecordWrite {
                    key,
                    expected: None,
                    value: Some(&[]),
                }],
                &self.control,
            )
            .map_err(|error| storage_error(self.operation, error))
    }

    pub(crate) fn finish(self) -> Result<PhysicalRows, SQLError> {
        self.snapshot()
    }

    pub(crate) fn snapshot(&self) -> Result<PhysicalRows, SQLError> {
        self.control.cancellation().check()?;
        let records = self
            .records
            .snapshot()
            .map_err(|error| storage_error(self.operation, error))?;
        let read_scope = records
            .read_scope(&self.control)
            .map_err(|error| storage_error(self.operation, error))?;
        Ok(PhysicalRows {
            records,
            _read_scope: read_scope,
            control: self.control.clone(),
            operation: self.operation,
        })
    }
}

/// Membership probes use exact ordered keys without traversing the other rows. Owning the frozen root keeps its authenticated spill alive independently of the builder.
pub(crate) struct PhysicalRows {
    records: PrivateRecordSnapshot,
    _read_scope: PrivateRecordReadScope,
    control: StorageReadControl,
    operation: &'static str,
}

impl PhysicalRows {
    /// Replay frozen keys in storage order without collecting the complete identity population.
    pub(crate) fn visit(
        &self,
        visit: &mut dyn FnMut(&str, DocId) -> Result<bool, SQLError>,
    ) -> Result<(), SQLError> {
        let mut failure = None;
        let result = self.records.visit(&[], None, &self.control, &mut |record| {
            let (table, id) = decode_physical_key(record.key())?;
            match visit(&table, id) {
                Ok(more) => Ok(more),
                Err(error) => {
                    failure = Some(error);
                    Ok(false)
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        result.map_err(|error| storage_error(self.operation, error))
    }

    pub(crate) fn contains(&self, table: &str, doc_id: DocId) -> Result<bool, SQLError> {
        self.control.cancellation().check()?;
        self.records
            .get(&physical_key(table, doc_id), &self.control)
            .map(|record| record.is_some())
            .map_err(|error| storage_error(self.operation, error))
    }
}

fn physical_key(table: &str, doc_id: DocId) -> Vec<u8> {
    // Escape embedded zero bytes and terminate the relation before the fixed unsigned identity. Ordered replay then agrees with (relation, identity), including relation-name prefixes.
    let mut key = Vec::with_capacity(table.len() + 10);
    for byte in table.bytes() {
        key.push(byte);
        if byte == 0 {
            key.push(255);
        }
    }
    key.extend_from_slice(&[0, 0]);
    key.extend_from_slice(&doc_id.to_be_bytes());
    key
}

fn decode_physical_key(key: &[u8]) -> Result<(std::borrow::Cow<'_, str>, DocId), VersionError> {
    let invalid = || VersionError::InvalidEncoding("invalid physical relation key");
    let width = key.len().checked_sub(10).ok_or_else(invalid)?;
    if key[width..width + 2] != [0, 0] {
        return Err(invalid());
    }
    let encoded = &key[..width];
    let table = if encoded.contains(&0) {
        let mut decoded = Vec::with_capacity(encoded.len());
        let mut bytes = encoded.iter().copied();
        while let Some(byte) = bytes.next() {
            if byte == 0 && bytes.next() != Some(255) {
                return Err(invalid());
            }
            decoded.push(byte);
        }
        std::borrow::Cow::Owned(String::from_utf8(decoded).map_err(|_| invalid())?)
    } else {
        std::borrow::Cow::Borrowed(std::str::from_utf8(encoded).map_err(|_| invalid())?)
    };
    let id = DocId::from_be_bytes(key[width + 2..].try_into().expect("fixed identity suffix"));
    Ok((table, id))
}

fn storage_error(operation: &str, error: VersionError) -> SQLError {
    crate::storage_errors::storage_error(operation, &error.into_storage_error())
}

#[cfg(test)]
mod tests;
