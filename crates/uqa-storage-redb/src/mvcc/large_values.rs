//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A committed large version owns a reference to immutable body bytes. Point reads follow the authoritative version reference independently of optional backup certificates.

use redb::{ReadableTable, ReadableTableMetadata, TableDefinition};
use uqa_storage::{
    backup::ImageDigest,
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::heads::INLINE_LIMIT;
use crate::error::redb_error;

pub(super) const BODIES: TableDefinition<&[u8; 32], &[u8]> =
    TableDefinition::new("uqa_mvcc_large_values");
pub(super) const OWNERS: TableDefinition<&[u8; 32], u64> =
    TableDefinition::new("uqa_mvcc_large_owners");
const REFERENCE: u8 = 2;

struct Reference<'a> {
    digest: &'a [u8; 32],
    size: u64,
}

fn reference(bytes: &[u8]) -> VersionResult<Option<Reference<'_>>> {
    if bytes.first() != Some(&REFERENCE) {
        return Ok(None);
    }
    let length = &bytes[1..];
    if length.len() != 40 {
        return Err(VersionError::InvalidEncoding(
            "invalid large record reference",
        ));
    }
    let size = u64::from_be_bytes(length[..8].try_into().expect("stored length"));
    if size <= INLINE_LIMIT as u64 {
        return Err(VersionError::InvalidEncoding("invalid large record length"));
    }
    Ok(Some(Reference {
        digest: length[8..].try_into().expect("complete digest"),
        size,
    }))
}

pub(super) fn tombstone(encoded: &[u8]) -> VersionResult<bool> {
    if reference(encoded)?.is_some() {
        return Ok(false);
    }
    Ok(super::codec::value_bytes(encoded)?.is_none())
}

pub(super) struct Writer<'a> {
    bodies: redb::Table<'a, &'static [u8; 32], &'static [u8]>,
    owners: redb::Table<'a, &'static [u8; 32], u64>,
}

impl<'a> Writer<'a> {
    pub(super) fn open(transaction: &'a redb::WriteTransaction) -> VersionResult<Self> {
        Ok(Self {
            bodies: transaction.open_table(BODIES).map_err(redb_error)?,
            owners: transaction.open_table(OWNERS).map_err(redb_error)?,
        })
    }

    pub(super) fn require_empty(&self) -> VersionResult<()> {
        if !self.bodies.is_empty().map_err(redb_error)?
            || !self.owners.is_empty().map_err(redb_error)?
        {
            return Err(VersionError::InvalidEncoding(
                "uninitialized metadata has record data",
            ));
        }
        Ok(())
    }

    /// A digest collision or a saturated owner count keeps an independent raw version.
    pub(super) fn store(
        &mut self,
        digest: ImageDigest,
        encoded: &[u8],
        control: Option<&StorageReadControl>,
    ) -> VersionResult<Option<[u8; 41]>> {
        let id = digest.as_bytes();
        let count = self.owners.get(&id).map_err(redb_error)?.map(|x| x.value());
        match count {
            Some(0) => return Err(VersionError::InvalidEncoding("zero large record owners")),
            Some(u64::MAX) => return Ok(None),
            Some(n) => {
                let existing = self
                    .bodies
                    .get(&id)
                    .map_err(redb_error)?
                    .ok_or(VersionError::InvalidEncoding("missing owned large record"))?;
                if existing.value().len() != encoded.len() {
                    return Ok(None);
                }
                for (a, b) in existing.value().chunks(65536).zip(encoded.chunks(65536)) {
                    if let Some(control) = control {
                        control.cancellation().check()?;
                    }
                    if a != b {
                        return Ok(None);
                    }
                }
                self.owners.insert(&id, n + 1).map_err(redb_error)?;
            }
            None => {
                if self
                    .bodies
                    .insert(&id, encoded)
                    .map_err(redb_error)?
                    .is_some()
                {
                    return Err(VersionError::InvalidEncoding("unowned large record"));
                }
                self.owners.insert(&id, 1).map_err(redb_error)?;
            }
        }
        let mut pointer = [0; 41];
        pointer[0] = REFERENCE;
        pointer[1..9].copy_from_slice(&((encoded.len() - 1) as u64).to_be_bytes());
        pointer[9..].copy_from_slice(&id);
        Ok(Some(pointer))
    }

    pub(super) fn release(
        transaction: &'a redb::WriteTransaction,
        values: &mut Option<Self>,
        encoded: &[u8],
    ) -> VersionResult<()> {
        let Some(reference) = reference(encoded)? else {
            return Ok(());
        };
        if values.is_none() {
            *values = Some(Self::open(transaction)?);
        }
        let writer = values.as_mut().expect("opened large record tables");
        let id = reference.digest;
        let n = writer
            .owners
            .get(id)
            .map_err(redb_error)?
            .map(|x| x.value())
            .ok_or(VersionError::InvalidEncoding("missing large record owners"))?;
        match n {
            0 => return Err(VersionError::InvalidEncoding("zero large record owners")),
            1 => {
                writer.owners.remove(id).map_err(redb_error)?;
                if writer.bodies.remove(id).map_err(redb_error)?.is_none() {
                    return Err(VersionError::InvalidEncoding("missing owned large record"));
                }
            }
            _ => {
                writer.owners.insert(id, n - 1).map_err(redb_error)?;
            }
        }
        Ok(())
    }
}

pub(super) struct Reader<'a> {
    txn: &'a redb::ReadTransaction,
    bodies: Option<redb::ReadOnlyTable<&'static [u8; 32], &'static [u8]>>,
}

impl<'a> Reader<'a> {
    pub(super) const fn new(txn: &'a redb::ReadTransaction) -> Self {
        Self { txn, bodies: None }
    }

    pub(super) fn with_value(
        &mut self,
        encoded: &[u8],
        visit: impl FnOnce(&[u8]) -> VersionResult<()>,
    ) -> VersionResult<()> {
        let Some(reference) = reference(encoded)? else {
            return visit(encoded);
        };
        if self.bodies.is_none() {
            self.bodies = Some(self.txn.open_table(BODIES).map_err(redb_error)?);
        }
        let data = self
            .bodies
            .as_ref()
            .expect("opened large records")
            .get(reference.digest)
            .map_err(redb_error)?
            .ok_or(VersionError::InvalidEncoding("missing owned large record"))?;
        let bytes = data.value();
        if bytes.first() != Some(&1) || (bytes.len() - 1) as u64 != reference.size {
            return Err(VersionError::InvalidEncoding(
                "large record length or tag mismatch",
            ));
        }
        visit(bytes)
    }
}
