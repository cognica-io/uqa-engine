//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-lived tagged byte leases shared by snapshot retention and SSI participation.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};

use parking_lot::Mutex;
use uqa_core::memory::MemoryReservation;
use uqa_storage::{
    mvcc::{DatabaseId, VersionError, VersionResult},
    native_file::{lock_would_block, read_exact_at, try_lock_byte, unlock_byte, write_all_at},
    read_control::StorageReadControl,
};

const ADMISSION: u64 = 0;
const HEADER: u64 = 8;
const HEADER_SIZE: usize = 32;
const SLOT_BASE: u64 = 64;
const SLOT_COUNT: u32 = 65_536;
const LEASE_BASE: u64 = 1 << 20;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct LeaseNamespace {
    pub(super) magic: [u8; 8],
    pub(super) database: DatabaseId,
    pub(super) incarnation: Option<[u8; 16]>,
}

struct State {
    file: File,
    namespace: LeaseNamespace,
    admitted: AtomicBool,
    occupied: Mutex<BTreeMap<u32, u64>>,
}

type Files = HashMap<PathBuf, Arc<State>>;
static FILES: OnceLock<Mutex<Files>> = OnceLock::new();

/// The registry retains one native descriptor until its last transport and lease are gone. Closing a second descriptor for the same file would release POSIX process locks, including locks owned by another live adapter.
#[derive(Clone)]
pub(super) struct NativeLeaseFile(Arc<State>);

pub(super) struct NativeLeaseAdmission(NativeLeaseFile);

impl Drop for NativeLeaseAdmission {
    fn drop(&mut self) {
        self.0.release_admission();
    }
}

struct Lease {
    state: Arc<State>,
    slot: u32,
    _memory: MemoryReservation,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut occupied = self.state.occupied.lock();
        // Lease destruction never takes admission or a physical writer. A failed unlock conservatively retains this slot until its descriptor closes.
        if unlock_byte(&self.state.file, LEASE_BASE + u64::from(self.slot)).is_ok() {
            occupied.remove(&self.slot);
        }
    }
}

fn io_error(error: std::io::Error) -> VersionError {
    VersionError::Storage(crate::SQLiteError::Io(error).into())
}

impl NativeLeaseFile {
    pub(super) fn open(path: &Path, namespace: LeaseNamespace) -> VersionResult<Self> {
        let mut files = FILES.get_or_init(Mutex::default).lock();
        // The map's strong reference also covers the interval between a weak owner's expiration and the completion of its destructor.
        files.retain(|_, state| Arc::strong_count(state) > 1);
        if let Some(state) = files.get(path) {
            if state.namespace != namespace {
                return Err(VersionError::WrongDatabase);
            }
            return Ok(Self(Arc::clone(state)));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(io_error)?;
        let state = Arc::new(State {
            file,
            namespace,
            admitted: AtomicBool::new(false),
            occupied: Mutex::new(BTreeMap::new()),
        });
        files.insert(path.to_owned(), Arc::clone(&state));
        Ok(Self(state))
    }

    pub(super) fn admit(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<NativeLeaseAdmission> {
        self.acquire_admission(control)?;
        Ok(NativeLeaseAdmission(self.clone()))
    }

    /// Split admission is used by the common snapshot transport's own scope guard.
    pub(super) fn acquire_admission(&self, control: &StorageReadControl) -> VersionResult<()> {
        loop {
            control.cancellation().check()?;
            if self
                .0
                .admitted
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let mut native_locked = false;
        let result = (|| {
            loop {
                control.cancellation().check()?;
                match try_lock_byte(&self.0.file, ADMISSION, true) {
                    Ok(()) => {
                        native_locked = true;
                        break;
                    }
                    Err(error) if lock_would_block(&error) => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => return Err(io_error(error)),
                }
            }
            let (namespace, count) = self.0.header()?;
            if namespace != self.0.namespace {
                let mut live = false;
                self.0.visit(count, control, &mut |_| {
                    live = true;
                    Ok(())
                })?;
                if live {
                    return Err(VersionError::WrongDatabase);
                }
                self.0.publish_header(0)?;
            } else if self.0.file.metadata().map_err(io_error)?.len() == 0 {
                self.0.publish_header(0)?;
            }
            Ok(())
        })();
        if result.is_err() {
            if native_locked {
                let _ = unlock_byte(&self.0.file, ADMISSION);
            }
            self.0.admitted.store(false, Ordering::Release);
        }
        result
    }

    pub(super) fn release_admission(&self) {
        let _ = unlock_byte(&self.0.file, ADMISSION);
        self.0.admitted.store(false, Ordering::Release);
    }

    /// Retain a tag while admitted. Reusing a free slot never makes its previous tag live again.
    pub(super) fn retain(
        &self,
        tag: u64,
        control: &StorageReadControl,
    ) -> VersionResult<Box<dyn Send + Sync>> {
        let memory = control
            .memory()
            .reserve(std::mem::size_of::<Lease>() + std::mem::size_of::<(u32, u64)>())?;
        let (_, count) = self.0.header()?;
        let mut occupied = self.0.occupied.lock();
        for slot in 0..SLOT_COUNT {
            control.cancellation().check()?;
            if occupied.contains_key(&slot) {
                continue;
            }
            let offset = LEASE_BASE + u64::from(slot);
            match try_lock_byte(&self.0.file, offset, true) {
                Ok(()) => {}
                Err(error) if lock_would_block(&error) => continue,
                Err(error) => return Err(io_error(error)),
            }
            let publish = (|| {
                write_all_at(
                    &self.0.file,
                    &tag.to_be_bytes(),
                    SLOT_BASE + u64::from(slot) * 8,
                )
                .map_err(io_error)?;
                if slot >= count {
                    self.0.publish_header(slot + 1)?;
                }
                Ok::<_, VersionError>(())
            })();
            if let Err(error) = publish {
                let _ = unlock_byte(&self.0.file, offset);
                return Err(error);
            }
            occupied.insert(slot, tag);
            return Ok(Box::new(Lease {
                state: Arc::clone(&self.0),
                slot,
                _memory: memory,
            }));
        }
        Err(VersionError::Storage(
            uqa_storage::StorageBackendError::Other("native lease capacity is exhausted".into()),
        ))
    }

    pub(super) fn visit(
        &self,
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(u64) -> VersionResult<()>,
    ) -> VersionResult<()> {
        let (_, count) = self.0.header()?;
        self.0.visit(count, control, visitor)
    }

    #[cfg(test)]
    pub(super) fn shares_descriptor(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl State {
    fn header(&self) -> VersionResult<(LeaseNamespace, u32)> {
        if self.file.metadata().map_err(io_error)?.len() == 0 {
            return Ok((self.namespace, 0));
        }
        let mut bytes = [0; HEADER_SIZE];
        read_exact_at(&self.file, &mut bytes, HEADER).map_err(io_error)?;
        let count = u32::from_be_bytes(bytes[24..28].try_into().expect("slot count"));
        if bytes[..8] != self.namespace.magic || count > SLOT_COUNT || bytes[28..] != [0; 4] {
            return Err(VersionError::InvalidEncoding("invalid native lease header"));
        }
        let incarnation = if self.namespace.incarnation.is_some() {
            let mut bytes = [0; 16];
            read_exact_at(&self.file, &mut bytes, HEADER + HEADER_SIZE as u64).map_err(io_error)?;
            Some(bytes)
        } else {
            None
        };
        Ok((
            LeaseNamespace {
                magic: self.namespace.magic,
                database: DatabaseId::from_bytes(
                    bytes[8..24].try_into().expect("database identity"),
                ),
                incarnation,
            },
            count,
        ))
    }

    fn publish_header(&self, count: u32) -> VersionResult<()> {
        let mut bytes = [0; HEADER_SIZE];
        bytes[..8].copy_from_slice(&self.namespace.magic);
        bytes[8..24].copy_from_slice(&self.namespace.database.as_bytes());
        bytes[24..28].copy_from_slice(&count.to_be_bytes());
        if let Some(incarnation) = self.namespace.incarnation {
            write_all_at(&self.file, &incarnation, HEADER + HEADER_SIZE as u64)
                .map_err(io_error)?;
        }
        write_all_at(&self.file, &bytes, HEADER).map_err(io_error)
    }

    fn visit(
        &self,
        count: u32,
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(u64) -> VersionResult<()>,
    ) -> VersionResult<()> {
        for slot in 0..count {
            control.cancellation().check()?;
            let occupied = self.occupied.lock();
            let tag = if let Some(tag) = occupied.get(&slot) {
                *tag
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
                u64::from_be_bytes(bytes)
            };
            // Visitors may release a lease, so never invoke one under the occupied-slot mutex.
            drop(occupied);
            visitor(tag)?;
        }
        Ok(())
    }
}
