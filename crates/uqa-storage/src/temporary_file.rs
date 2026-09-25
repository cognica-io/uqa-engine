//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ephemeral authenticated byte files with independent reader positions and bounded workspace.

use std::io::{self, IoSlice, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chacha20poly1305::{
    aead::{AeadInPlace, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use parking_lot::Mutex;

const BLOCK_BYTES: usize = 16 * 1024;
const NONCE_BYTES: usize = 24;
const LENGTH_BYTES: usize = 2;
const TAG_BYTES: usize = 16;
const SLOT_HEADER_BYTES: usize = NONCE_BYTES + LENGTH_BYTES + TAG_BYTES;
#[cfg(test)]
const RECORD_BYTES: usize = 1 + 2 * (SLOT_HEADER_BYTES + BLOCK_BYTES);

/// Ordinary spill files use 16 KiB logical blocks.
pub type TemporaryFile = BlockTemporaryFile<BLOCK_BYTES>;

/// A private temporary byte file with a compile-time logical block width of 1 through 16,384 bytes. Each physical block reserves two authenticated ciphertext slots and a one-byte active selector: `1 + 2 * (24 + 2 + 16 + BYTES)` bytes. Only the populated prefix is encrypted; its two-byte length and block position are authenticated. An incomplete replacement never overwrites the active slot. Only ciphertext and its public framing reach disk; each file has a fresh random key which exists solely in this owner. Reopened handles share the owner but have independent logical positions. The last handle removes the file. Abrupt process death may leave an encrypted file whose key was never persisted; this is not a durable recovery or anti-replay format.
pub struct BlockTemporaryFile<const BYTES: usize> {
    owner: Arc<Mutex<Owner<BYTES>>>,
    path: Arc<PathBuf>,
    position: u64,
}

struct Owner<const BYTES: usize> {
    file: tempfile::NamedTempFile,
    cipher: XChaCha20Poly1305,
    length: u64,
    #[cfg(test)]
    faults: tests::Faults,
}

pub struct TemporaryFileMetadata {
    length: u64,
}

impl TemporaryFileMetadata {
    pub fn len(&self) -> u64 {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl<const BYTES: usize> BlockTemporaryFile<BYTES> {
    /// Ciphertext file length for a logical length, including both slots and complete block framing. This excludes filesystem allocation metadata.
    pub fn physical_len_for(logical_length: u64) -> io::Result<u64> {
        if BYTES == 0 || BYTES > BLOCK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid temporary file block width",
            ));
        }
        record_offset::<BYTES>(logical_length.div_ceil(BYTES as u64))
    }

    pub fn new() -> io::Result<Self> {
        Self::from_file(tempfile::NamedTempFile::new()?)
    }

    pub fn new_in(directory: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_file(tempfile::NamedTempFile::new_in(directory)?)
    }

    fn from_file(file: tempfile::NamedTempFile) -> io::Result<Self> {
        Self::physical_len_for(0)?;
        let mut key = [0_u8; 32];
        getrandom::fill(&mut key).map_err(|error| io::Error::other(error.to_string()))?;
        let cipher = XChaCha20Poly1305::new((&key).into());
        key.fill(0);
        let path = Arc::new(file.path().to_path_buf());
        Ok(Self {
            owner: Arc::new(Mutex::new(Owner {
                file,
                cipher,
                length: 0,
                #[cfg(test)]
                faults: tests::Faults::default(),
            })),
            path,
            position: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn fail_write_after(&self, bytes: usize) {
        self.owner.lock().faults.fail_after_bytes = Some(bytes);
    }

    #[cfg(test)]
    pub(crate) fn block_io_counts(&self) -> (usize, u64) {
        let owner = self.owner.lock();
        (owner.faults.read_blocks, owner.faults.written_bytes)
    }

    pub fn as_file(&self) -> &Self {
        self
    }

    pub fn as_file_mut(&mut self) -> &mut Self {
        self
    }

    /// Write the complete concatenation of the slices, sharing one authenticated block publication across adjacent fields. Like `Write::write_all`, an error can leave a written prefix; record owners retain responsibility for rolling back incomplete records.
    pub fn write_all_vectored(&mut self, mut input: &mut [IoSlice<'_>]) -> io::Result<()> {
        IoSlice::advance_slices(&mut input, 0);
        while !input.is_empty() {
            match self.write_vectored(input) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "incomplete temporary file write",
                    ));
                }
                Ok(written) => IoSlice::advance_slices(&mut input, written),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn reopen(&self) -> io::Result<Self> {
        self.owner.lock().validate_length()?;
        Ok(Self {
            owner: Arc::clone(&self.owner),
            path: Arc::clone(&self.path),
            position: 0,
        })
    }

    pub fn metadata(&self) -> io::Result<TemporaryFileMetadata> {
        Ok(TemporaryFileMetadata {
            length: self.owner.lock().length,
        })
    }

    pub fn set_len(&self, length: u64) -> io::Result<()> {
        record_offset::<BYTES>(length.div_ceil(BYTES as u64))?;
        let mut owner = self.owner.lock();
        if length >= owner.length {
            return owner.extend_zeroed(length);
        }
        // Bytes beyond the new logical end are hidden, not rewritten. Growth zeroes them before publication; a failed truncate cannot corrupt the retained partial block.
        owner.truncate_physical(record_offset::<BYTES>(length.div_ceil(BYTES as u64))?)?;
        owner.length = length;
        Ok(())
    }
}

fn record_offset<const BYTES: usize>(block: u64) -> io::Result<u64> {
    block
        .checked_mul((1 + 2 * (SLOT_HEADER_BYTES + BYTES)) as u64)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary file offset overflow",
            )
        })
}

fn block_aad(block: u64, length: u16) -> [u8; 8 + LENGTH_BYTES] {
    let mut aad = [0; 8 + LENGTH_BYTES];
    aad[..8].copy_from_slice(&block.to_le_bytes());
    aad[8..].copy_from_slice(&length.to_le_bytes());
    aad
}

impl<const BYTES: usize> Owner<BYTES> {
    fn validate_length(&self) -> io::Result<()> {
        let expected = record_offset::<BYTES>(self.length.div_ceil(BYTES as u64))?;
        if self.file.as_file().metadata()?.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid authenticated temporary file length",
            ));
        }
        Ok(())
    }

    fn active_slot(&mut self, block: u64) -> io::Result<u8> {
        let file = self.file.as_file_mut();
        file.seek(SeekFrom::Start(record_offset::<BYTES>(block)?))?;
        let mut active = [0];
        file.read_exact(&mut active)?;
        if active[0] > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid temporary file active slot",
            ));
        }
        Ok(active[0])
    }

    fn read_block(&mut self, block: u64) -> io::Result<[u8; BYTES]> {
        let mut plaintext = [0_u8; BYTES];
        if block < self.length.div_ceil(BYTES as u64) {
            #[cfg(test)]
            {
                self.faults.read_blocks += 1;
            }
            let active = self.active_slot(block)?;
            let file = self.file.as_file_mut();
            file.seek(SeekFrom::Start(slot_offset::<BYTES>(block, active)?))?;
            let mut nonce = [0_u8; NONCE_BYTES];
            let mut length = [0_u8; LENGTH_BYTES];
            let mut tag = [0_u8; TAG_BYTES];
            file.read_exact(&mut nonce)?;
            file.read_exact(&mut length)?;
            let length = u16::from_le_bytes(length);
            let populated = usize::from(length);
            let required = (self.length - block * BYTES as u64).min(BYTES as u64) as usize;
            if populated > BYTES || populated < required {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid authenticated temporary block length",
                ));
            }
            file.read_exact(&mut tag)?;
            file.read_exact(&mut plaintext[..populated])?;
            self.cipher
                .decrypt_in_place_detached(
                    XNonce::from_slice(&nonce),
                    &block_aad(block, length),
                    &mut plaintext[..populated],
                    (&tag).into(),
                )
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "temporary file authentication failed",
                    )
                })?;
        }
        Ok(plaintext)
    }

    fn write_block(
        &mut self,
        block: u64,
        ciphertext: &mut [u8; BYTES],
        populated: usize,
    ) -> io::Result<()> {
        let existing = block < self.length.div_ceil(BYTES as u64);
        let next = if existing {
            self.active_slot(block)? ^ 1
        } else {
            0
        };
        let mut nonce = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        let length = u16::try_from(populated).expect("temporary blocks are at most 16 KiB");
        let tag = self
            .cipher
            .encrypt_in_place_detached(
                XNonce::from_slice(&nonce),
                &block_aad(block, length),
                &mut ciphertext[..populated],
            )
            .map_err(|_| io::Error::other("temporary file encryption failed"))?;
        // The complete replacement reaches the inactive slot before its one-byte publication marker. Failed short writes leave the authoritative slot unchanged, including the prefix retained by append rollback.
        self.file
            .as_file_mut()
            .seek(SeekFrom::Start(slot_offset::<BYTES>(block, next)?))?;
        self.write_physical(&nonce)?;
        self.write_physical(&length.to_le_bytes())?;
        self.write_physical(&tag)?;
        self.write_physical(&ciphertext[..populated])?;
        if !existing {
            self.truncate_physical(record_offset::<BYTES>(block + 1)?)?;
        }
        self.file
            .as_file_mut()
            .seek(SeekFrom::Start(record_offset::<BYTES>(block)?))?;
        self.write_physical(&[next])
    }

    fn write_physical(&mut self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        self.faults.before_write(self.file.as_file_mut(), bytes)?;
        self.file.as_file_mut().write_all(bytes)
    }

    fn truncate_physical(&mut self, length: u64) -> io::Result<()> {
        #[cfg(test)]
        self.faults.before_truncate()?;
        self.file.as_file().set_len(length)
    }

    fn rollback_length(&mut self, length: u64, cause: &io::Error) -> io::Result<()> {
        // Logical visibility returns to the original boundary even when the filesystem cannot remove an unpublished ciphertext tail. Existing readers keep that prefix; reopen rejects inconsistent physical length and the caller receives the cleanup error.
        self.length = length;
        self.truncate_physical(record_offset::<BYTES>(length.div_ceil(BYTES as u64))?)
            .map_err(|rollback| {
                io::Error::other(format!(
                    "{cause}; temporary file rollback failed: {rollback}"
                ))
            })?;
        Ok(())
    }

    fn extend_zeroed(&mut self, length: u64) -> io::Result<()> {
        let original = self.length;
        let result = (|| {
            while self.length < length {
                let block = self.length / BYTES as u64;
                let offset = (self.length % BYTES as u64) as usize;
                let mut bytes = self.read_block(block)?;
                let count = (length - self.length).min((BYTES - offset) as u64) as usize;
                bytes[offset..offset + count].fill(0);
                self.write_block(block, &mut bytes, offset + count)?;
                self.length += count as u64;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.rollback_length(original, &error)?;
            return Err(error);
        }
        Ok(())
    }
}

fn slot_offset<const BYTES: usize>(block: u64, slot: u8) -> io::Result<u64> {
    record_offset::<BYTES>(block)?
        .checked_add(1 + u64::from(slot) * (SLOT_HEADER_BYTES + BYTES) as u64)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary file slot offset overflow",
            )
        })
}

impl<const BYTES: usize> Read for BlockTemporaryFile<BYTES> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut owner = self.owner.lock();
        if self.position >= owner.length || output.is_empty() {
            return Ok(0);
        }
        let block = self.position / BYTES as u64;
        let offset = (self.position % BYTES as u64) as usize;
        let bytes = owner.read_block(block)?;
        let count =
            (output.len().min(BYTES - offset) as u64).min(owner.length - self.position) as usize;
        output[..count].copy_from_slice(&bytes[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl<const BYTES: usize> Write for BlockTemporaryFile<BYTES> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.write_vectored(&[IoSlice::new(input)])
    }

    fn write_vectored(&mut self, input: &[IoSlice<'_>]) -> io::Result<usize> {
        if input.iter().all(|bytes| bytes.is_empty()) {
            return Ok(0);
        }
        let minimum_end = self.position.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary file length overflow",
            )
        })?;
        record_offset::<BYTES>(minimum_end.div_ceil(BYTES as u64))?;
        let mut owner = self.owner.lock();
        let original = owner.length;
        owner.extend_zeroed(self.position)?;
        let block = self.position / BYTES as u64;
        let offset = (self.position % BYTES as u64) as usize;
        let mut bytes = match owner.read_block(block) {
            Ok(bytes) => bytes,
            Err(error) => {
                owner.rollback_length(original, &error)?;
                return Err(error);
            }
        };
        let mut count = 0;
        for source in input {
            let taken = source.len().min(BYTES - offset - count);
            bytes[offset + count..offset + count + taken].copy_from_slice(&source[..taken]);
            count += taken;
            if offset + count == BYTES {
                break;
            }
        }
        let next_position = self.position.checked_add(count as u64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary file length overflow",
            )
        })?;
        let populated =
            (owner.length.max(next_position) - block * BYTES as u64).min(BYTES as u64) as usize;
        if let Err(error) = owner.write_block(block, &mut bytes, populated) {
            owner.rollback_length(original, &error)?;
            return Err(error);
        }
        owner.length = owner.length.max(next_position);
        self.position = next_position;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.owner.lock().file.as_file_mut().flush()
    }
}

impl<const BYTES: usize> Seek for BlockTemporaryFile<BYTES> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(position) => position,
            SeekFrom::End(offset) => self
                .owner
                .lock()
                .length
                .checked_add_signed(offset)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid temporary file seek")
                })?,
            SeekFrom::Current(offset) => {
                self.position.checked_add_signed(offset).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid temporary file seek")
                })?
            }
        };
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests;
