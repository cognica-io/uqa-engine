//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Read, Write};

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use crate::read_control::StorageReadControl;

use super::frame::{read_exact, require_end, write_all};
use super::{
    BackupError, BackupLimits, BackupResult, ImageDigest, ImageFormat, ImageProvider, NamespaceId,
    ObjectSpan, UnitAddress, UnitKind,
};

const MAGIC: &[u8; 8] = b"UQAINV01";
const ADDRESS_BYTES: usize = 30;
const VALUE_BYTES: usize = 1 + 8 + 32 + ObjectSpan::ENCODED_BYTES;

struct OwnedAddress {
    namespace: NamespaceId,
    generation: u64,
    family: u16,
    key: BudgetedVec<u8>,
}

impl OwnedAddress {
    fn copy(
        address: UnitAddress<'_>,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        limits.check_key(address.key.len())?;
        if address.family == 0 {
            return Err(BackupError::Invalid("zero unit family"));
        }
        let mut key = BudgetedVec::new(control.memory());
        key.reserve(address.key.len())?;
        for chunk in address.key.chunks(16 * 1024) {
            control.cancellation().check()?;
            key.extend_from_slice(chunk)?;
        }
        Ok(Self {
            namespace: address.namespace,
            generation: address.generation,
            family: address.family,
            key,
        })
    }

    fn view(&self) -> UnitAddress<'_> {
        UnitAddress {
            namespace: self.namespace,
            generation: self.generation,
            family: self.family,
            key: &self.key,
        }
    }
}

/// One owned inventory entry. Its key retains the supplied allowance; object references have fixed size regardless of unit payload length.
pub struct InventoryEntry {
    address: OwnedAddress,
    kind: UnitKind,
    decoded_bytes: u64,
    digest: ImageDigest,
    object: ObjectSpan,
}

impl InventoryEntry {
    pub fn new(
        address: UnitAddress<'_>,
        kind: UnitKind,
        decoded_bytes: u64,
        digest: ImageDigest,
        object: ObjectSpan,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        validate_length(decoded_bytes, object, limits)?;
        Ok(Self {
            address: OwnedAddress::copy(address, limits, control)?,
            kind,
            decoded_bytes,
            digest,
            object,
        })
    }

    pub fn address(&self) -> UnitAddress<'_> {
        self.address.view()
    }
    pub const fn kind(&self) -> UnitKind {
        self.kind
    }
    pub const fn decoded_bytes(&self) -> u64 {
        self.decoded_bytes
    }
    pub const fn digest(&self) -> ImageDigest {
        self.digest
    }
    pub const fn object(&self) -> ObjectSpan {
        self.object
    }
}

fn validate_length(
    decoded_bytes: u64,
    object: ObjectSpan,
    limits: BackupLimits,
) -> BackupResult<()> {
    let framing = object.frames() * super::frame::MIN_FRAME_BYTES as u64;
    if decoded_bytes > object.encoded_bytes() - framing
        || decoded_bytes.div_ceil(u64::from(limits.max_frame_bytes())) > object.frames()
    {
        return Err(BackupError::Invalid("unit length exceeds framed span"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventorySummary {
    pub units: u64,
    pub decoded_bytes: u64,
    pub image_digest: ImageDigest,
}

impl InventorySummary {
    pub(super) const ENCODED_BYTES: usize = 48;

    pub(super) fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0; Self::ENCODED_BYTES];
        bytes[..8].copy_from_slice(&self.units.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.decoded_bytes.to_le_bytes());
        bytes[16..].copy_from_slice(&self.image_digest.as_bytes());
        bytes
    }

    pub(super) fn decode(bytes: &[u8; Self::ENCODED_BYTES]) -> BackupResult<Self> {
        let summary = Self {
            units: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            decoded_bytes: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            image_digest: ImageDigest::from_bytes(bytes[16..].try_into().unwrap()),
        };
        if summary.units == 0 && summary.decoded_bytes != 0 {
            return Err(BackupError::Invalid("empty inventory byte count"));
        }
        Ok(summary)
    }
}

struct Accumulator {
    digest: Sha256,
    units: u64,
    bytes: u64,
    previous: Option<OwnedAddress>,
}

impl Accumulator {
    fn new(format: ImageFormat) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"UQA backup logical image\0\x01");
        digest.update(format.bytes());
        Self {
            digest,
            units: 0,
            bytes: 0,
            previous: None,
        }
    }

    fn admit(
        &mut self,
        entry: &InventoryEntry,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| previous.view() >= entry.address())
        {
            return Err(BackupError::Invalid("duplicate or unordered unit address"));
        }
        let next_units = self
            .units
            .checked_add(1)
            .ok_or(BackupError::Invalid("unit count overflow"))?;
        let next_bytes = self
            .bytes
            .checked_add(entry.decoded_bytes)
            .ok_or(BackupError::Invalid("image size overflow"))?;
        let previous = OwnedAddress::copy(entry.address(), limits, control)?;
        self.digest.update(address_bytes(entry.address()));
        for chunk in entry.address().key.chunks(16 * 1024) {
            control.cancellation().check()?;
            self.digest.update(chunk);
        }
        self.digest.update([entry.kind as u8]);
        self.digest.update(entry.decoded_bytes.to_le_bytes());
        self.digest.update(entry.digest.as_bytes());
        self.units = next_units;
        self.bytes = next_bytes;
        self.previous = Some(previous);
        Ok(())
    }

    fn summary(&self) -> InventorySummary {
        InventorySummary {
            units: self.units,
            decoded_bytes: self.bytes,
            image_digest: ImageDigest::from_bytes(self.digest.clone().finalize().into()),
        }
    }
}

fn address_bytes(address: UnitAddress<'_>) -> [u8; ADDRESS_BYTES] {
    let mut bytes = [0; ADDRESS_BYTES];
    bytes[..16].copy_from_slice(&address.namespace.as_bytes());
    bytes[16..24].copy_from_slice(&address.generation.to_le_bytes());
    bytes[24..26].copy_from_slice(&address.family.to_le_bytes());
    bytes[26..].copy_from_slice(&(address.key.len() as u32).to_le_bytes());
    bytes
}

/// Streaming inventory writer. Any append error permanently fails this writer; no partial stream can subsequently obtain a successful footer. The output owner supplies protection and durable publication separately.
pub struct InventoryWriter<W> {
    output: W,
    limits: BackupLimits,
    control: StorageReadControl,
    accumulator: Accumulator,
    failed: bool,
}

impl<W: Write> InventoryWriter<W> {
    pub fn new(
        mut output: W,
        format: ImageFormat,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let mut header = [0; 16];
        header[..8].copy_from_slice(MAGIC);
        header[8..10].copy_from_slice(&1_u16.to_le_bytes());
        header[10] = format.provider() as u8;
        header[12..].copy_from_slice(&format.version().to_le_bytes());
        write_all(&mut output, &header, control)?;
        Ok(Self {
            output,
            limits,
            control: control.clone(),
            accumulator: Accumulator::new(format),
            failed: false,
        })
    }

    pub fn append(&mut self, entry: &InventoryEntry) -> BackupResult<()> {
        if self.failed {
            return Err(BackupError::Finished);
        }
        let result = self.append_inner(entry);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn append_inner(&mut self, entry: &InventoryEntry) -> BackupResult<()> {
        self.accumulator.admit(entry, self.limits, &self.control)?;
        write_all(&mut self.output, &[1], &self.control)?;
        write_all(
            &mut self.output,
            &address_bytes(entry.address()),
            &self.control,
        )?;
        write_all(&mut self.output, entry.address().key, &self.control)?;
        let mut value = [0; VALUE_BYTES];
        value[0] = entry.kind as u8;
        value[1..9].copy_from_slice(&entry.decoded_bytes.to_le_bytes());
        value[9..41].copy_from_slice(&entry.digest.as_bytes());
        value[41..].copy_from_slice(&entry.object.encode());
        write_all(&mut self.output, &value, &self.control)
    }

    pub fn finish(mut self) -> BackupResult<(W, InventorySummary)> {
        if self.failed {
            return Err(BackupError::Finished);
        }
        let summary = self.accumulator.summary();
        write_all(&mut self.output, &[0], &self.control)?;
        write_all(&mut self.output, &summary.encode(), &self.control)?;
        Ok((self.output, summary))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadState {
    Open,
    Complete,
    Failed,
}

/// Ordered bounded inventory reader. A summary becomes available only after the complete footer, counts, digest and end of input have been verified. It does not read referenced payload objects.
pub struct InventoryReader<R> {
    input: R,
    limits: BackupLimits,
    control: StorageReadControl,
    format: ImageFormat,
    accumulator: Accumulator,
    state: ReadState,
}

impl<R: Read> InventoryReader<R> {
    pub fn new(
        mut input: R,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let mut header = [0; 16];
        read_exact(&mut input, &mut header, control)?;
        if &header[..8] != MAGIC || header[11] != 0 {
            return Err(BackupError::Invalid("inventory header"));
        }
        if header[8..10] != 1_u16.to_le_bytes() {
            return Err(BackupError::Unsupported("inventory version"));
        }
        let format = ImageFormat::new(
            ImageProvider::decode(header[10])?,
            u32::from_le_bytes(header[12..].try_into().unwrap()),
        )?;
        Ok(Self {
            input,
            limits,
            control: control.clone(),
            format,
            accumulator: Accumulator::new(format),
            state: ReadState::Open,
        })
    }

    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    pub fn summary(&self) -> Option<InventorySummary> {
        (self.state == ReadState::Complete).then(|| self.accumulator.summary())
    }

    pub fn read_next(&mut self) -> BackupResult<Option<InventoryEntry>> {
        match self.state {
            ReadState::Complete => return Ok(None),
            ReadState::Failed => return Err(BackupError::Finished),
            ReadState::Open => {}
        }
        let result = self.read_inner();
        if result.is_err() {
            self.state = ReadState::Failed;
        }
        result
    }

    fn read_inner(&mut self) -> BackupResult<Option<InventoryEntry>> {
        let mut tag = [0];
        read_exact(&mut self.input, &mut tag, &self.control)?;
        if tag[0] == 0 {
            let mut bytes = [0; InventorySummary::ENCODED_BYTES];
            read_exact(&mut self.input, &mut bytes, &self.control)?;
            if InventorySummary::decode(&bytes)? != self.accumulator.summary() {
                return Err(BackupError::Integrity);
            }
            require_end(&mut self.input, &self.control)?;
            self.state = ReadState::Complete;
            return Ok(None);
        }
        if tag[0] != 1 {
            return Err(BackupError::Invalid("inventory entry tag"));
        }
        let mut bytes = [0; ADDRESS_BYTES];
        read_exact(&mut self.input, &mut bytes, &self.control)?;
        let namespace = NamespaceId::from_bytes(bytes[..16].try_into().unwrap())?;
        let generation = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let family = u16::from_le_bytes(bytes[24..26].try_into().unwrap());
        if family == 0 {
            return Err(BackupError::Invalid("zero unit family"));
        }
        let key_bytes = u32::from_le_bytes(bytes[26..].try_into().unwrap()) as usize;
        self.limits.check_key(key_bytes)?;
        let mut key = BudgetedVec::new(self.control.memory());
        key.reserve(key_bytes)?;
        let zeros = [0; 1024];
        while key.len() < key_bytes {
            self.control.cancellation().check()?;
            key.extend_from_slice(&zeros[..(key_bytes - key.len()).min(zeros.len())])?;
        }
        read_exact(&mut self.input, &mut key, &self.control)?;
        let mut value = [0; VALUE_BYTES];
        read_exact(&mut self.input, &mut value, &self.control)?;
        let entry = InventoryEntry {
            address: OwnedAddress {
                namespace,
                generation,
                family,
                key,
            },
            kind: UnitKind::decode(value[0])?,
            decoded_bytes: u64::from_le_bytes(value[1..9].try_into().unwrap()),
            digest: ImageDigest::from_bytes(value[9..41].try_into().unwrap()),
            object: ObjectSpan::decode(&value[41..])?,
        };
        validate_length(entry.decoded_bytes, entry.object, self.limits)?;
        self.accumulator.admit(&entry, self.limits, &self.control)?;
        Ok(Some(entry))
    }
}
