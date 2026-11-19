//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared record-image decoding validates envelope, revisions and complete repository integrity.

use uqa_core::memory::BudgetedVec;

use crate::{
    backup::{
        BackupCut, BackupError, BackupImageReader, BackupLeaseInput, BackupReadStep, BackupResult,
        BackupRoot, BackupSourceStep, ImageFormat, NamespaceId, UnitKind,
    },
    mvcc::{CommitSequence, RECLAMATION_DOMAIN_PREFIX, RECLAMATION_EPOCH_NAMESPACE},
    read_control::StorageReadControl,
};

/// A provider mapping validates its fixed header and original record keys. Its metadata must remain immutable after header admission, and validation must use only the supplied input and original control.
pub trait BackupRecordImportMapping {
    fn format(&self) -> ImageFormat;
    fn namespace(&self) -> NamespaceId;
    fn header_bytes(&self) -> usize;
    fn decode_header(
        &mut self,
        bytes: &[u8],
        cut: BackupCut,
        control: &StorageReadControl,
    ) -> BackupResult<()>;
    fn validate_record_key(&self, key: &[u8], control: &StorageReadControl) -> BackupResult<()>;
}

/// The current unit's revision or watermark has already been consumed and checked. Record payloads remain provisional until their end and complete repository verification.
#[derive(Clone, Copy, Debug)]
pub enum RecordImageImportUnit<'a> {
    Identifier {
        namespace: &'a [u8],
        watermark: u64,
    },
    Record {
        key: &'a [u8],
        revision: CommitSequence,
        value_bytes: u64,
    },
}

/// Checks the mapping envelope, ordered header, allocator exclusions and record revisions while sharing the repository's complete integrity verifier. Logical catalog and artifact validation remain with their respective format owners before target publication.
pub struct MappedRecordImageImportReader<'a, M: BackupRecordImportMapping> {
    reader: Option<BackupImageReader<'a>>,
    root: BackupRoot,
    mapping: Option<M>,
    control: StorageReadControl,
    header: bool,
    current: Option<u64>,
    complete: bool,
}

impl<'a, M: BackupRecordImportMapping> MappedRecordImageImportReader<'a, M> {
    pub fn new(
        lease: impl Into<BackupLeaseInput<'a>>,
        mapping: M,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let lease = lease.into();
        let root = lease.root();
        let cut = root.cut();
        if cut.format != mapping.format() || cut.sequence.is_none() || mapping.header_bytes() == 0 {
            return Err(BackupError::Unsupported("record import image format"));
        }
        Ok(Self {
            reader: Some(lease.read_image(maximum_workspace_bytes, control)?),
            root,
            mapping: Some(mapping),
            control: control.clone(),
            header: false,
            current: None,
            complete: false,
        })
    }

    /// The admitted mapping is retained only while input is active; completion or failure releases its charged metadata.
    pub fn mapping(&self) -> Option<&M> {
        self.mapping.as_ref()
    }

    pub const fn root(&self) -> BackupRoot {
        self.root
    }

    pub fn current_unit(&self) -> Option<RecordImageImportUnit<'_>> {
        let value = self.current?;
        let entry = self.reader.as_ref()?.current_unit()?;
        let key = entry.address().key;
        Some(if entry.address().family == 2 {
            RecordImageImportUnit::Identifier {
                namespace: key,
                watermark: value,
            }
        } else {
            RecordImageImportUnit::Record {
                key,
                revision: CommitSequence::from_u64(value),
                value_bytes: entry.decoded_bytes() - 8,
            }
        })
    }

    /// Consume one envelope or one repository verification window. Record values must be drained through their verified end before advancing; identifiers have no unread payload.
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupSourceStep> {
        let result = self.advance_inner(maximum_object_bytes);
        if result.is_err() && !self.complete {
            self.release(false);
        }
        result
    }

    fn advance_inner(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        if self.complete {
            return Ok(BackupSourceStep::Complete);
        }
        let reader = self.reader.as_mut().ok_or(BackupError::Finished)?;
        self.current = None;
        match reader.advance(maximum)? {
            BackupReadStep::Pending => return Ok(BackupSourceStep::Pending),
            BackupReadStep::Complete => {
                if !self.header || reader.verified().is_none() {
                    return Err(BackupError::Invalid("missing record image header"));
                }
                self.release(true);
                return Ok(BackupSourceStep::Complete);
            }
            BackupReadStep::Unit => (),
        }
        let entry = reader.current_unit().expect("selected repository unit");
        let address = entry.address();
        if address.namespace != self.mapping.as_ref().unwrap().namespace()
            || address.generation != 0
        {
            return Err(BackupError::Invalid("record image address"));
        }
        if !self.header {
            if address.family != 1
                || !address.key.is_empty()
                || entry.kind() != UnitKind::Metadata
                || entry.decoded_bytes() != self.mapping.as_ref().unwrap().header_bytes() as u64
            {
                return Err(BackupError::Invalid("record image header unit"));
            }
            let length = self.mapping.as_ref().unwrap().header_bytes();
            let mut bytes = BudgetedVec::new(self.control.memory());
            bytes.reserve(length)?;
            for _ in 0..length {
                bytes.push(0)?;
            }
            reader.read_exact_current(&mut bytes)?;
            reader.require_current_end()?;
            self.mapping
                .as_mut()
                .unwrap()
                .decode_header(&bytes, self.root.cut(), &self.control)?;
            self.header = true;
            return Ok(BackupSourceStep::Pending);
        }
        match address.family {
            2 if entry.kind() == UnitKind::Metadata && entry.decoded_bytes() == 8 => {
                if address.key.is_empty()
                    || address.key == RECLAMATION_EPOCH_NAMESPACE
                    || address.key.starts_with(RECLAMATION_DOMAIN_PREFIX)
                {
                    return Err(BackupError::Invalid("record image identifier namespace"));
                }
            }
            3 if entry.kind() == UnitKind::Record && entry.decoded_bytes() >= 8 => {
                self.mapping
                    .as_ref()
                    .unwrap()
                    .validate_record_key(address.key, &self.control)?;
            }
            _ => return Err(BackupError::Invalid("record image unit family or length")),
        }
        let identifier = address.family == 2;
        let mut bytes = [0; 8];
        reader.read_exact_current(&mut bytes)?;
        let value = u64::from_be_bytes(bytes);
        if identifier {
            reader.require_current_end()?;
        } else if value == 0 || value > self.root.cut().sequence.unwrap().as_u64() {
            return Err(BackupError::Invalid("record image record revision"));
        }
        self.current = Some(value);
        Ok(BackupSourceStep::Unit)
    }

    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let result = if matches!(
            self.current_unit(),
            Some(RecordImageImportUnit::Record { .. })
        ) {
            self.reader.as_mut().unwrap().read_current(output)
        } else {
            Err(BackupError::Finished)
        };
        if result.is_err() && !self.complete {
            self.release(false);
        }
        result
    }

    fn release(&mut self, complete: bool) {
        self.complete = complete;
        self.reader = None;
        self.current = None;
        self.mapping = None;
    }
}
