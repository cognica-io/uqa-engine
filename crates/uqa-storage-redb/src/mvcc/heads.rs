//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current small payloads share their directory row. Out-of-line payloads and displaced revisions keep their snapshot visibility independently of the digest index.

use redb::{ReadableTable, ReadableTableMetadata, TableDefinition};
use uqa_storage::{
    backup::ImageDigest,
    mvcc::{CommitSequence, PreparedRecordCommit, VersionError, VersionResult},
    read_control::StorageReadControl,
};

use super::{
    image_fingerprints::{TABLE, UNCERTIFIED_HISTORY},
    HEADS, VERSIONS,
};
use crate::error::redb_error;

pub(super) const LEGACY_HEADS: TableDefinition<&[u8], u64> = TableDefinition::new("uqa_mvcc_heads");
mod encoding;
#[cfg(test)]
pub(super) use encoding::EMPTY_IMAGE_HASHES;
pub(super) use encoding::{empty_fingerprint, RecordHead, INLINE_LIMIT};
use encoding::{encode_certificate, Revision};

pub(super) fn stored_size(revision: u64, length: usize) -> VersionResult<usize> {
    Revision::new(revision).size(Some(length))
}

/// Fill the native reservation from the source reader before publishing the inline row or immutable body reference.
pub(super) fn insert_from<E>(
    images: &mut VersionWriter<'_>,
    heads: &mut redb::Table<'_, &'static [u8], &'static [u8]>,
    versions: &mut redb::Table<'_, (&'static [u8], u64), &'static [u8]>,
    key: &[u8],
    revision: u64,
    length: usize,
    read: impl FnOnce(&mut [u8]) -> Result<ImageDigest, E>,
) -> Result<(), E>
where
    E: From<VersionError> + From<uqa_storage::StorageBackendError>,
{
    let number = Revision::new(revision);
    let size = number.size(Some(length))?;
    let inline = length <= INLINE_LIMIT;
    let prefix = size - length - 1;
    let mut entry = if inline {
        heads.insert_reserve(key, size).map_err(redb_error)?
    } else {
        versions
            .insert_reserve((key, revision), size)
            .map_err(redb_error)?
    };
    let output = entry.as_mut();
    output[prefix] = 1;
    let digest = read(&mut output[prefix + 1..])?;
    if inline {
        number.inline(output, (length != 0).then_some(digest));
    } else {
        drop(entry);
        let pointer = {
            let value = versions
                .get((key, revision))
                .map_err(redb_error)?
                .expect("reserved imported version");
            images.large(key, revision, value.value(), digest)?
        };
        if let Some(reference) = pointer {
            versions
                .insert((key, revision), reference.as_slice())
                .map_err(redb_error)?;
        }
        heads
            .insert(key, number.indexed_live(&mut [0; 9]))
            .map_err(redb_error)?;
    }
    Ok(())
}

type FingerprintHistory<'a> = redb::Table<'a, (&'static [u8], u64), &'static [u8; 40]>;

/// Large current values and displaced revisions share one exact (key, revision) certificate.
pub(super) struct VersionWriter<'a> {
    transaction: &'a redb::WriteTransaction,
    table: Option<FingerprintHistory<'a>>,
    bodies: Option<super::large_values::Writer<'a>>,
    control: Option<&'a StorageReadControl>,
}

impl<'a> VersionWriter<'a> {
    pub(super) fn new(
        transaction: &'a redb::WriteTransaction,
        control: Option<&'a StorageReadControl>,
    ) -> Self {
        Self {
            transaction,
            table: None,
            bodies: None,
            control,
        }
    }

    fn table(&mut self) -> VersionResult<&mut FingerprintHistory<'a>> {
        if self.table.is_none() {
            self.table = Some(self.transaction.open_table(TABLE).map_err(redb_error)?);
        }
        Ok(self.table.as_mut().expect("opened revision image table"))
    }

    fn store(&mut self, key: &[u8], revision: u64, image: &[u8; 40]) -> VersionResult<()> {
        self.table()?
            .insert((key, revision), image)
            .map_err(redb_error)?;
        Ok(())
    }

    fn large(
        &mut self,
        key: &[u8],
        revision: u64,
        encoded: &[u8],
        digest: ImageDigest,
    ) -> VersionResult<Option<[u8; 41]>> {
        self.store(
            key,
            revision,
            &encode_certificate(encoded.len() - 1, digest)?,
        )?;
        if self.bodies.is_none() {
            self.bodies = Some(super::large_values::Writer::open(self.transaction)?);
        }
        self.bodies
            .as_mut()
            .expect("opened immutable bodies")
            .store(digest, encoded, self.control)
    }

    fn displace(&mut self, key: &[u8], previous: &RecordHead<'_>) -> VersionResult<()> {
        if previous.indexed_live {
            let entry = self
                .table()?
                .get((key, previous.revision))
                .map_err(redb_error)?;
            if entry.is_some_and(|value| {
                super::image_fingerprints::large_certificate(value.value()).is_some()
            }) {
                return Ok(());
            }
        }
        self.store(
            key,
            previous.revision,
            previous
                .history_certificate()?
                .as_ref()
                .unwrap_or(&UNCERTIFIED_HISTORY),
        )
    }
}

pub(super) fn initialize_empty_heads(
    transaction: &redb::WriteTransaction,
    has_legacy_heads: bool,
) -> VersionResult<()> {
    if !transaction
        .open_table(HEADS)
        .map_err(redb_error)?
        .is_empty()
        .map_err(redb_error)?
        || !transaction
            .open_table(TABLE)
            .map_err(redb_error)?
            .is_empty()
            .map_err(redb_error)?
    {
        return Err(VersionError::InvalidEncoding(
            "uninitialized metadata has record data",
        ));
    }
    if has_legacy_heads {
        let old = transaction.open_table(LEGACY_HEADS).map_err(redb_error)?;
        if !old.is_empty().map_err(redb_error)? {
            return Err(VersionError::InvalidEncoding(
                "uninitialized metadata has record data",
            ));
        }
        transaction.delete_table(old).map_err(redb_error)?;
    }
    Ok(())
}

pub(super) fn write_batch(
    transaction: &redb::WriteTransaction,
    heads: &mut redb::Table<'_, &'static [u8], &'static [u8]>,
    sequence: CommitSequence,
    prepared: &PreparedRecordCommit,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let number = Revision::new(sequence.as_u64());
    let mut maximum = 0;
    let mut writes = prepared.writes();
    while let Some(write) = writes.next_metadata(control)? {
        let len = write
            .value_len()
            .map(usize::try_from)
            .transpose()
            .map_err(|_| VersionError::InvalidEncoding("record size overflow"))?;
        maximum = maximum.max(number.size(len)?);
    }
    let mut workspace = control.memory().reserve(maximum)?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(maximum)
        .map_err(|error| uqa_storage::StorageBackendError::Memory(error.into()))?;
    workspace.grow(encoded.capacity() - maximum)?;
    let mut versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
    let mut images = VersionWriter::new(transaction, Some(control));
    let mut writes = prepared.writes();
    while let Some(write) = writes.next(control)? {
        let value = write.value();
        // redb's insert_reserve allocates a temporary value for every row. Reuse one charged encoding buffer for this bounded physical commit instead.
        encoded.clear();
        let inline = value.is_none_or(|bytes| bytes.len() <= INLINE_LIMIT);
        if inline {
            let len = value.map(<[u8]>::len);
            encoded.resize(number.size(len)? - len.unwrap_or_default() - 1, 0);
        }
        encoded.push(u8::from(value.is_some()));
        let fingerprint = if let Some(value) = value.filter(|bytes| !bytes.is_empty()) {
            let mut hash = uqa_storage::backup::RecordImageHasher::new(sequence);
            for source in value.chunks(65536) {
                control.cancellation().check()?;
                encoded.extend_from_slice(source);
                hash.update(source);
            }
            Some(hash.finish())
        } else {
            None
        };
        let mut external_head = [0; 9];
        let directory = if inline {
            number.inline(&mut encoded, fingerprint);
            encoded.as_slice()
        } else {
            let reference = images.large(
                write.key(),
                sequence.as_u64(),
                &encoded,
                fingerprint.expect("external live value"),
            )?;
            versions
                .insert(
                    (write.key(), sequence.as_u64()),
                    reference
                        .as_ref()
                        .map_or(encoded.as_slice(), <[u8; 41]>::as_slice),
                )
                .map_err(redb_error)?;
            number.indexed_live(&mut external_head)
        };
        if let Some(previous) = heads.insert(write.key(), directory).map_err(redb_error)? {
            let previous = RecordHead::decode(previous.value())?;
            if let Some(inline) = previous.inline {
                versions
                    .insert((write.key(), previous.revision), inline)
                    .map_err(redb_error)?;
            }
            images.displace(write.key(), &previous)?;
        }
    }
    Ok(())
}

/// Adopt the original head revisions without loading record values. Older records without a certificate retain ordinary visibility and bounded content comparison.
pub(super) fn migrate_heads(
    transaction: &redb::WriteTransaction,
    format: u64,
) -> VersionResult<()> {
    let old = transaction.open_table(LEGACY_HEADS).map_err(redb_error)?;
    let mut heads = transaction.open_table(HEADS).map_err(redb_error)?;
    let mut images = transaction.open_table(TABLE).map_err(redb_error)?;
    for entry in old.iter().map_err(redb_error)? {
        let (key, previous) = entry.map_err(redb_error)?;
        let revision = previous.value();
        let certificate = if format == 65 {
            images
                .remove((key.value(), revision))
                .map_err(redb_error)?
                .map(|value| *value.value())
        } else {
            None
        };
        heads
            .insert(
                key.value(),
                Revision::new(revision).external(certificate.as_ref(), &mut [0; 49]),
            )
            .map_err(redb_error)?;
    }
    drop(old);
    transaction.delete_table(LEGACY_HEADS).map_err(redb_error)?;
    Ok(())
}
