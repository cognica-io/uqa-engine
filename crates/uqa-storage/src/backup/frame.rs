//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;

use chacha20poly1305::{
    aead::{AeadInOut, KeyInit},
    XChaCha20Poly1305,
};
use cipher::zeroize::Zeroize;
use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use crate::read_control::StorageReadControl;

use super::{BackupError, BackupLimits, BackupResult, ObjectId, ProtectionId, RepositoryId};

const MAGIC: &[u8; 8] = b"UQAFRM01";
const HEADER_BYTES: usize = 96;
pub(super) const MIN_FRAME_BYTES: usize = HEADER_BYTES + 16;
const IO_WINDOW: usize = 16 * 1024;

struct KeyMaterial([u8; 32]);

impl Drop for KeyMaterial {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A caller-owned repository key, separate from source database credentials. Clones retain the same non-serializable key; the last owner zeroizes its bytes.
#[derive(Clone)]
pub struct BackupKey(Arc<KeyMaterial>);

impl BackupKey {
    pub fn new(mut bytes: [u8; 32]) -> Self {
        let key = Self(Arc::new(KeyMaterial(bytes)));
        bytes.zeroize();
        key
    }
}

impl fmt::Debug for BackupKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BackupKey([REDACTED])")
    }
}

/// Plaintext integrity is not authentication. An encrypted repository protects inventory and roots with the same framing as data.
#[derive(Clone, Debug)]
pub enum Protection {
    Plaintext,
    Encrypted {
        domain: ProtectionId,
        key: BackupKey,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Unit = 1,
    Inventory = 2,
    Root = 3,
    Repository = 4,
    Intent = 5,
    RestoreIntent = 6,
    RestoreInitialized = 7,
    RestoreStarted = 8,
    RestorePrepared = 9,
    RestoreCancelled = 10,
    RestoreComplete = 11,
}

/// Exact immutable object and byte position supplied by the owning object reader/writer, not trusted from a frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameContext {
    pub repository: RepositoryId,
    pub object: ObjectId,
    pub offset: u64,
    pub kind: FrameKind,
}

fn header(
    context: FrameContext,
    protection: &Protection,
    bytes: u32,
    nonce: &[u8; 24],
) -> [u8; HEADER_BYTES] {
    let mut header = [0; HEADER_BYTES];
    header[..8].copy_from_slice(MAGIC);
    header[8..10].copy_from_slice(&1_u16.to_le_bytes());
    if let Protection::Encrypted { domain, .. } = protection {
        header[10] = 1;
        header[28..44].copy_from_slice(&domain.as_bytes());
    }
    header[11] = context.kind as u8;
    header[12..28].copy_from_slice(&context.repository.as_bytes());
    header[44..60].copy_from_slice(&context.object.as_bytes());
    header[60..68].copy_from_slice(&context.offset.to_le_bytes());
    header[68..72].copy_from_slice(&bytes.to_le_bytes());
    header[72..].copy_from_slice(nonce);
    header
}

pub(super) fn encoded_bytes(
    context: FrameContext,
    protection: &Protection,
    decoded: usize,
) -> BackupResult<u64> {
    let trailer = match protection {
        Protection::Plaintext => 32,
        Protection::Encrypted { .. } => 16,
    };
    let encoded = HEADER_BYTES as u64 + decoded as u64 + trailer;
    context
        .offset
        .checked_add(encoded)
        .ok_or(BackupError::Invalid("frame offset overflow"))?;
    Ok(encoded)
}

/// Encode one bounded frame. A failed write may leave a prefix; the repository owner must discard that incomplete object rather than publish or append to it. No completion/durability is implied by success here.
pub fn write_frame<W: Write>(
    output: &mut W,
    context: FrameContext,
    protection: &Protection,
    payload: &[u8],
    limits: BackupLimits,
    control: &StorageReadControl,
) -> BackupResult<u64> {
    control.cancellation().check()?;
    limits.check_frame(payload.len())?;
    let size = encoded_bytes(context, protection, payload.len())?;
    let mut nonce = [0; 24];
    if matches!(protection, Protection::Encrypted { .. }) {
        getrandom::fill(&mut nonce).map_err(|_| BackupError::Random)?;
    }
    let header = header(context, protection, payload.len() as u32, &nonce);
    match protection {
        Protection::Plaintext => {
            let mut digest = Sha256::new();
            digest.update(header);
            for chunk in payload.chunks(IO_WINDOW) {
                control.cancellation().check()?;
                digest.update(chunk);
            }
            write_all(output, &header, control)?;
            write_all(output, payload, control)?;
            write_all(output, &digest.finalize(), control)?;
        }
        Protection::Encrypted { key, .. } => {
            let mut ciphertext = BudgetedVec::new(control.memory());
            ciphertext.reserve(payload.len())?;
            for chunk in payload.chunks(IO_WINDOW) {
                control.cancellation().check()?;
                ciphertext.extend_from_slice(chunk)?;
            }
            let cipher = XChaCha20Poly1305::new((&key.0 .0).into());
            let tag = cipher
                .encrypt_inout_detached((&nonce).into(), &header, (&mut *ciphertext).into())
                .map_err(|_| BackupError::Integrity)?;
            write_all(output, &header, control)?;
            write_all(output, &ciphertext, control)?;
            write_all(output, &tag, control)?;
        }
    }
    Ok(size)
}

/// Read exactly one frame, checking its expected context before allocation. The returned plaintext retains the invoking memory allowance. Object and unit digest verification remain the enclosing reader's responsibility.
pub fn read_frame<R: Read>(
    input: &mut R,
    context: FrameContext,
    protection: &Protection,
    limits: BackupLimits,
    control: &StorageReadControl,
) -> BackupResult<BudgetedVec<u8>> {
    let mut encoded_header = [0; HEADER_BYTES];
    read_exact(input, &mut encoded_header, control)?;
    if &encoded_header[..8] != MAGIC {
        return Err(BackupError::Invalid("frame magic"));
    }
    if encoded_header[8..10] != 1_u16.to_le_bytes() {
        return Err(BackupError::Unsupported("frame version"));
    }
    let size = u32::from_le_bytes(encoded_header[68..72].try_into().unwrap()) as usize;
    limits.check_frame(size)?;
    encoded_bytes(context, protection, size)?;
    let nonce: [u8; 24] = encoded_header[72..].try_into().unwrap();
    if matches!(protection, Protection::Plaintext) && nonce != [0; 24] {
        return Err(BackupError::Invalid("plaintext nonce"));
    }
    if encoded_header != header(context, protection, size as u32, &nonce) {
        return Err(BackupError::Invalid("frame context or protection domain"));
    }
    let mut payload = BudgetedVec::new(control.memory());
    payload.reserve(size)?;
    let zeros = [0; IO_WINDOW];
    while payload.len() < size {
        control.cancellation().check()?;
        let count = (size - payload.len()).min(IO_WINDOW);
        payload.extend_from_slice(&zeros[..count])?;
    }
    read_exact(input, &mut payload, control)?;
    match protection {
        Protection::Plaintext => {
            let mut expected = [0; 32];
            read_exact(input, &mut expected, control)?;
            let mut digest = Sha256::new();
            digest.update(encoded_header);
            for chunk in payload.chunks(IO_WINDOW) {
                control.cancellation().check()?;
                digest.update(chunk);
            }
            if <[u8; 32]>::from(digest.finalize()) != expected {
                return Err(BackupError::Integrity);
            }
        }
        Protection::Encrypted { key, .. } => {
            let mut tag = [0; 16];
            read_exact(input, &mut tag, control)?;
            let cipher = XChaCha20Poly1305::new((&key.0 .0).into());
            if cipher
                .decrypt_inout_detached(
                    (&nonce).into(),
                    &encoded_header,
                    (&mut *payload).into(),
                    (&tag).into(),
                )
                .is_err()
            {
                payload.zeroize();
                return Err(BackupError::Integrity);
            }
        }
    }
    control.cancellation().check()?;
    Ok(payload)
}

pub(super) fn write_all(
    output: &mut impl Write,
    mut bytes: &[u8],
    control: &StorageReadControl,
) -> BackupResult<()> {
    while !bytes.is_empty() {
        control.cancellation().check()?;
        let count = bytes.len().min(IO_WINDOW);
        match output.write(&bytes[..count]) {
            Ok(0) => {
                return Err(
                    io::Error::new(io::ErrorKind::WriteZero, "incomplete backup write").into(),
                )
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn read_exact(
    input: &mut impl Read,
    mut bytes: &mut [u8],
    control: &StorageReadControl,
) -> BackupResult<()> {
    control.cancellation().check()?;
    while !bytes.is_empty() {
        control.cancellation().check()?;
        let count = bytes.len().min(IO_WINDOW);
        match input.read(&mut bytes[..count]) {
            Ok(0) => {
                return Err(
                    io::Error::new(io::ErrorKind::UnexpectedEof, "truncated backup input").into(),
                )
            }
            Ok(read) => bytes = &mut bytes[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn require_end(input: &mut impl Read, control: &StorageReadControl) -> BackupResult<()> {
    let mut byte = [0];
    loop {
        control.cancellation().check()?;
        match input.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(BackupError::Invalid("trailing inventory bytes")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}
