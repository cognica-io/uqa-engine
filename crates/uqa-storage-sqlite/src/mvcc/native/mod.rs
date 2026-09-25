//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native `SQLite` row mapping, atomic baseline conversion and evaluated commit materialization. The record adapter preserves existing physical tables while shared storage owns private changes and visibility; native catalog/store session routing remains separate.

mod capture;
mod diskann;
mod family;
mod format;
mod graph_cache;
mod graph_guards;
mod graph_lifetimes;
mod graph_lookup;
mod key;
mod layout;
mod maintenance;
mod notifications;
pub(super) use maintenance::NativeMaintenanceRecords;
mod occurrence_guards;
mod owners;
mod physical;
mod projection;
mod queue;
mod row;
mod sequences;
mod session;
mod standalone_graph;
mod vector_guards;

pub(crate) use session::NativeSnapshot;

pub(super) use format::validate_restoration;
pub(super) use format::{check_mapping, initialize, initialize_in, present, reject_mapped};
pub(super) use projection::materialize;

#[cfg(test)]
mod tests;

pub use family::NativeRecordFamily;
pub use key::{NativeRecordIdentity, NativeRecordOwner};
pub use layout::{NativeColumnType, NativeRecordLayout};
pub use row::{decode_row, encode_row};

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{CommitSequence, DatabaseId, RecordWrite, VersionError, VersionResult};
use uqa_storage::read_control::StorageReadControl;

/// Stable data addressing, independent of the transaction history incarnation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NativeRecordNamespace(pub(super) DatabaseId);

pub(super) struct NativeMapping {
    pub(super) identity: DatabaseId,
    pub(super) namespace: NativeRecordNamespace,
}

/// One evaluated native row in the provider's record encoding. Its charged buffers can be borrowed by common storage's conditional commit preparation.
#[derive(Debug)]
pub struct NativeRecord {
    key: BudgetedVec<u8>,
    row: BudgetedVec<u8>,
}

impl NativeRecord {
    pub fn encode(
        family: NativeRecordFamily,
        owner: NativeRecordOwner,
        values: &[ValueRef<'_>],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let layout = family.layout();
        let identity = NativeRecordIdentity::new(family, owner)?;
        identity.validate_row(values)?;
        let mut components = BudgetedVec::new(control.memory());
        for &column in layout.identity_columns {
            components.push(values[column])?;
        }
        let key = identity.encode_key(&components, control)?;
        let row = encode_row(values, control)?;
        Ok(Self { key, row })
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn row(&self) -> &[u8] {
        &self.row
    }

    pub fn write(&self, expected: Option<CommitSequence>) -> RecordWrite<'_> {
        RecordWrite {
            key: self.key(),
            expected,
            value: Some(self.row()),
        }
    }
}

/// Validate a retained row against its full native key and return borrowed column values. This rejects a correct-looking payload addressed to a different tuple or family before physical materialization.
pub fn decode_record<'a>(
    key: &[u8],
    row: &'a [u8],
    control: &StorageReadControl,
) -> VersionResult<(NativeRecordIdentity, BudgetedVec<ValueRef<'a>>)> {
    control.cancellation().check()?;
    let identity = NativeRecordIdentity::decode(key)?;
    let layout = identity.family().layout();
    let values = decode_row(row, layout.columns.len(), control)?;
    identity.validate_row(&values)?;
    let mut components = BudgetedVec::new(control.memory());
    for &column in layout.identity_columns {
        components.push(values[column])?;
    }
    if identity.encode_key(&components, control)?.as_ref() != key {
        return Err(invalid("native row does not match its record key"));
    }
    Ok((identity, values))
}

fn invalid(message: &'static str) -> VersionError {
    VersionError::InvalidEncoding(message)
}
