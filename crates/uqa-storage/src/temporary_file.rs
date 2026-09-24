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
const TAG_BYTES: usize = 16;
#[cfg(test)]
const RECORD_BYTES: usize = 1 + 2 * (NONCE_BYTES + BLOCK_BYTES + TAG_BYTES);

/// Ordinary spill files use 16 KiB logical blocks.
pub type TemporaryFile = BlockTemporaryFile<BLOCK_BYTES>;

/// A private temporary byte file with a compile-time logical block width of 1 through 16,384 bytes. Each physical block reserves two authenticated ciphertext slots and a one-byte active selector: `1 + 2 * (24 + BYTES + 16)` bytes. An incomplete replacement never overwrites the active slot. Only ciphertext, nonces, authentication tags and selectors reach disk; each file has a fresh random key which exists solely in this owner. Reopened handles share the owner but have independent logical positions. The last handle removes the file. Abrupt process death may leave an encrypted file whose key was never persisted; this is not a durable recovery or anti-replay format.
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
    pub fn new() -> io::Result<Self> {
        Self::from_file(tempfile::NamedTempFile::new()?)
    }

    pub fn new_in(directory: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_file(tempfile::NamedTempFile::new_in(directory)?)
    }

    fn from_file(file: tempfile::NamedTempFile) -> io::Result<Self> {
        if BYTES == 0 || BYTES > BLOCK_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid temporary file block width",
            ));
        }
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

    pub fn as_file(&self) -> &Self {
        self
    }

    pub fn as_file_mut(&mut self) -> &mut Self {
        self
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
        .checked_mul((1 + 2 * (NONCE_BYTES + BYTES + TAG_BYTES)) as u64)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary file offset overflow",
            )
        })
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
            let mut tag = [0_u8; TAG_BYTES];
            file.read_exact(&mut nonce)?;
            file.read_exact(&mut plaintext)?;
            file.read_exact(&mut tag)?;
            self.cipher
                .decrypt_in_place_detached(
                    XNonce::from_slice(&nonce),
                    &block.to_le_bytes(),
                    &mut plaintext,
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

    fn write_block(&mut self, block: u64, ciphertext: &mut [u8; BYTES]) -> io::Result<()> {
        let existing = block < self.length.div_ceil(BYTES as u64);
        let next = if existing {
            self.active_slot(block)? ^ 1
        } else {
            0
        };
        let mut nonce = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        let tag = self
            .cipher
            .encrypt_in_place_detached(XNonce::from_slice(&nonce), &block.to_le_bytes(), ciphertext)
            .map_err(|_| io::Error::other("temporary file encryption failed"))?;
        // The complete replacement reaches the inactive slot before its one-byte publication marker. Failed short writes leave the authoritative slot unchanged, including the prefix retained by append rollback.
        self.file
            .as_file_mut()
            .seek(SeekFrom::Start(slot_offset::<BYTES>(block, next)?))?;
        self.write_physical(&nonce)?;
        self.write_physical(ciphertext)?;
        self.write_physical(&tag)?;
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
                self.write_block(block, &mut bytes)?;
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
        .checked_add(1 + u64::from(slot) * (NONCE_BYTES + BYTES + TAG_BYTES) as u64)
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
        if let Err(error) = owner.write_block(block, &mut bytes) {
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
