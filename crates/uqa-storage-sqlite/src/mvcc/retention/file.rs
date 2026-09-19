//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Snapshot sequences use a separate provider-owned file with process-lived byte leases. No payload, key or credential is written here.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;
use uqa_core::memory::MemoryReservation;
use uqa_storage::mvcc::{
    CommitSequence, DatabaseId, SnapshotLeaseTransport, SnapshotRegistry, VersionError,
    VersionResult,
};
use uqa_storage::native_file::{
    lock_would_block, read_exact_at, try_lock_byte, unlock_byte, write_all_at,
};
use uqa_storage::read_control::StorageReadControl;

const ADMISSION: u64 = 0;
const HEADER: u64 = 8;
const HEADER_SIZE: usize = 32;
const MAGIC: &[u8; 8] = b"UQASNP01";
const SLOT_BASE: u64 = 64;
const SLOT_COUNT: u32 = 65_536;
const LEASE_BASE: u64 = 1 << 20;

struct RegistryEntry {
    identity: DatabaseId,
    registry: Weak<SnapshotRegistry>,
    state: Arc<State>,
}

type Registries = HashMap<PathBuf, RegistryEntry>;
static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();

#[cfg(test)]
mod tests;

pub(super) fn registry(path: &Path, identity: DatabaseId) -> VersionResult<Arc<SnapshotRegistry>> {
    let path = super::database_path(path)?;
    let mut registries = REGISTRIES.get_or_init(Mutex::default).lock();
    // A failed Weak::upgrade can race the old registry's destructor. Keep the descriptor until its last transport/lease owner has finished, or reuse that same descriptor; closing it after opening a replacement would release the replacement's POSIX locks.
    registries.retain(|_, entry| {
        entry.registry.strong_count() != 0 || Arc::strong_count(&entry.state) > 1
    });
    if let Some(entry) = registries.get(&path) {
        if let Some(registry) = entry.registry.upgrade() {
            if entry.identity != identity {
                return Err(VersionError::WrongDatabase);
            }
            return Ok(registry);
        }
    }
    let state = if let Some(entry) = registries.get(&path) {
        if entry.identity != identity {
            return Err(VersionError::WrongDatabase);
        }
        Arc::clone(&entry.state)
    } else {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(".uqa-snapshots");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(sidecar)
            .map_err(io_error)?;
        Arc::new(State {
            file,
            identity,
            occupied: Mutex::new(BTreeMap::new()),
        })
    };
    let transport = Arc::new(Transport {
        state: Arc::clone(&state),
    });
    let registry = Arc::new(SnapshotRegistry::with_transport(transport));
    registries.insert(
        path,
        RegistryEntry {
            identity,
            registry: Arc::downgrade(&registry),
            state,
        },
    );
    Ok(registry)
}

fn io_error(error: std::io::Error) -> VersionError {
    VersionError::Storage(crate::SQLiteError::Io(error).into())
}

struct State {
    file: File,
    identity: DatabaseId,
    occupied: Mutex<BTreeMap<u32, CommitSequence>>,
}

struct Transport {
    state: Arc<State>,
}

struct Lease {
    state: Arc<State>,
    slot: u32,
    _memory: MemoryReservation,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut occupied = self.state.occupied.lock();
        // A failed release leaves a conservative live entry until the owner closes; it must never advertise an unprotected reusable slot.
        if unlock_byte(&self.state.file, LEASE_BASE + u64::from(self.slot)).is_ok() {
            occupied.remove(&self.slot);
        }
    }
}

impl State {
    fn header(&self) -> VersionResult<([u8; 16], u32)> {
        let mut bytes = [0; HEADER_SIZE];
        if self.file.metadata().map_err(io_error)?.len() == 0 {
            return Ok((self.identity.as_bytes(), 0));
        }
        read_exact_at(&self.file, &mut bytes, HEADER).map_err(io_error)?;
        let count = u32::from_be_bytes(bytes[24..28].try_into().expect("slot count"));
        if &bytes[..8] != MAGIC || count > SLOT_COUNT || bytes[28..] != [0; 4] {
            return Err(VersionError::InvalidEncoding(
                "invalid snapshot lease header",
            ));
        }
        Ok((bytes[8..24].try_into().expect("database identity"), count))
    }

    fn publish_header(&self, count: u32) -> VersionResult<()> {
        let mut bytes = [0; HEADER_SIZE];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..24].copy_from_slice(&self.identity.as_bytes());
        bytes[24..28].copy_from_slice(&count.to_be_bytes());
        write_all_at(&self.file, &bytes, HEADER).map_err(io_error)
    }

    fn oldest(
        &self,
        count: u32,
        control: &StorageReadControl,
    ) -> VersionResult<Option<CommitSequence>> {
        let mut oldest = None;
        for slot in 0..count {
            control.cancellation().check()?;
            let occupied = self.occupied.lock();
            let sequence = if let Some(sequence) = occupied.get(&slot) {
                *sequence
            } else {
                let offset = LEASE_BASE + u64::from(slot);
                match try_lock_byte(&self.file, offset, true) {
                    Ok(()) => {
                        unlock_byte(&self.file, offset).map_err(io_error)?;
                        continue;
                    }
                    Err(error) if lock_would_block(&error) => {}
                    Err(error) => return Err(io_error(error)),
                }
                let mut bytes = [0; 8];
                read_exact_at(&self.file, &mut bytes, SLOT_BASE + u64::from(slot) * 8)
                    .map_err(io_error)?;
                CommitSequence::from_u64(u64::from_be_bytes(bytes))
            };
            oldest = Some(oldest.map_or(sequence, |old: CommitSequence| old.min(sequence)));
        }
        Ok(oldest)
    }
}

impl SnapshotLeaseTransport for Transport {
    fn acquire_admission(&self, control: &StorageReadControl) -> VersionResult<()> {
        loop {
            control.cancellation().check()?;
            match try_lock_byte(&self.state.file, ADMISSION, true) {
                Ok(()) => break,
                Err(error) if lock_would_block(&error) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => return Err(io_error(error)),
            }
        }
        let result = (|| {
            let (identity, count) = self.state.header()?;
            if identity != self.state.identity.as_bytes() {
                if self.state.oldest(count, control)?.is_some() {
                    return Err(VersionError::WrongDatabase);
                }
                self.state.publish_header(0)?;
            } else if self.state.file.metadata().map_err(io_error)?.len() == 0 {
                self.state.publish_header(0)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.release_admission();
        }
        result
    }

    fn release_admission(&self) {
        let _ = unlock_byte(&self.state.file, ADMISSION);
    }

    fn retain(
        &self,
        sequence: CommitSequence,
        control: &StorageReadControl,
    ) -> VersionResult<Box<dyn Send + Sync>> {
        let memory = control
            .memory()
            .reserve(std::mem::size_of::<Lease>() + std::mem::size_of::<(u32, CommitSequence)>())?;
        let (_, count) = self.state.header()?;
        let mut occupied = self.state.occupied.lock();
        for slot in 0..SLOT_COUNT {
            control.cancellation().check()?;
            if occupied.contains_key(&slot) {
                continue;
            }
            let offset = LEASE_BASE + u64::from(slot);
            match try_lock_byte(&self.state.file, offset, true) {
                Ok(()) => {}
                Err(error) if lock_would_block(&error) => continue,
                Err(error) => return Err(io_error(error)),
            }
            let publish = (|| {
                write_all_at(
                    &self.state.file,
                    &sequence.as_u64().to_be_bytes(),
                    SLOT_BASE + u64::from(slot) * 8,
                )
                .map_err(io_error)?;
                if slot >= count {
                    self.state.publish_header(slot + 1)?;
                }
                Ok::<_, VersionError>(())
            })();
            if let Err(error) = publish {
                let _ = unlock_byte(&self.state.file, offset);
                return Err(error);
            }
            occupied.insert(slot, sequence);
            return Ok(Box::new(Lease {
                state: Arc::clone(&self.state),
                slot,
                _memory: memory,
            }));
        }
        Err(VersionError::Storage(
            uqa_storage::StorageBackendError::Other("snapshot lease capacity is exhausted".into()),
        ))
    }

    fn oldest(&self, control: &StorageReadControl) -> VersionResult<Option<CommitSequence>> {
        let (_, count) = self.state.header()?;
        self.state.oldest(count, control)
    }
}
