//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-lived dependency references have native leases, separate from transaction locks.

use super::{
    lock_would_block, read_exact_at, write_all_at, FileLockCoordinator, HOLDER_SLOT_BASE,
    HOLDER_SLOT_COUNT, HOLDER_SLOT_SIZE,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

const ADMISSION_BYTE: u64 = 15;
const HEADER_BASE: u64 = HOLDER_SLOT_BASE + HOLDER_SLOT_COUNT * HOLDER_SLOT_SIZE;
const HEADER_SIZE: u64 = 16;
const SLOT_BASE: u64 = HEADER_BASE + HEADER_SIZE;
const SLOT_SIZE: u64 = 32;
const SLOT_COUNT: u32 = super::super::super::temporary_roles::MAX_TEMPORARY_ROLE_REFERENCES;
const LEASE_BASE: u64 = SLOT_BASE + SLOT_SIZE * SLOT_COUNT as u64;
const HEADER_MAGIC: u32 = 0x5551_5452;
const SLOT_MAGIC: u32 = 0x5551_5453;
const FORMAT: u32 = 1;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct Slots {
    by_key: BTreeMap<(u64, u32), u32>,
    by_slot: BTreeSet<u32>,
    next: u32,
}

struct Admission<'a>(&'a FileLockCoordinator);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        let _ = self.0.apply_byte_mode(ADMISSION_BYTE, Some(true), None);
    }
}

impl FileLockCoordinator {
    fn temporary_role_admission(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Admission<'_>, SQLError> {
        loop {
            cancel.check()?;
            match self.apply_byte_mode(ADMISSION_BYTE, None, Some(true)) {
                Ok(()) => return Ok(Admission(self)),
                Err(error) if lock_would_block(&error) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => {
                    return Err(SQLError::Internal(format!(
                        "acquire temporary role dependency coordination: {error}"
                    )))
                }
            }
        }
    }

    fn temporary_role_high_water(&self) -> Result<u32, SQLError> {
        let mut bytes = [0; HEADER_SIZE as usize];
        match read_exact_at(&self.file, &mut bytes, HEADER_BASE) {
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                let length = self
                    .file
                    .metadata()
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "inspect temporary role dependency header: {error}"
                        ))
                    })?
                    .len();
                if length <= HEADER_BASE {
                    return Ok(0);
                }
                return Err(SQLError::Internal(
                    "truncated temporary role dependency header".into(),
                ));
            }
            Err(error) => {
                return Err(SQLError::Internal(format!(
                    "read temporary role dependency header: {error}"
                )))
            }
            Ok(()) => {}
        }
        if bytes == [0; HEADER_SIZE as usize] {
            return Ok(0);
        }
        let word =
            |offset| u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("header word"));
        if word(0) != HEADER_MAGIC || word(4) != FORMAT || word(8) > SLOT_COUNT || word(12) != 0 {
            return Err(SQLError::Internal(
                "invalid temporary role dependency header".into(),
            ));
        }
        Ok(word(8))
    }

    pub(in crate::row_locks) fn retain_temporary_role(
        &self,
        session: u64,
        role: u32,
        cancel: &CancellationToken,
    ) -> Result<(), SQLError> {
        let mut local = self.temporary_role_slots.lock();
        if local.by_key.contains_key(&(session, role)) {
            return Ok(());
        }
        let _admission = self.temporary_role_admission(cancel)?;
        let high_water = self.temporary_role_high_water()?;
        for distance in 0..SLOT_COUNT {
            cancel.check()?;
            let slot = (local.next + distance) % SLOT_COUNT;
            if local.by_slot.contains(&slot) {
                continue;
            }
            let lease = LEASE_BASE + u64::from(slot);
            match self.apply_byte_mode(lease, None, Some(true)) {
                Err(error) if lock_would_block(&error) => continue,
                Err(error) => {
                    return Err(SQLError::Internal(format!(
                        "retain temporary role dependency lease: {error}"
                    )))
                }
                Ok(()) => {}
            }
            let stored = (|| {
                let mut bytes = [0; SLOT_SIZE as usize];
                bytes[..4].copy_from_slice(&SLOT_MAGIC.to_be_bytes());
                bytes[4..8].copy_from_slice(&role.to_be_bytes());
                bytes[8..12].copy_from_slice(&std::process::id().to_be_bytes());
                bytes[12..16].copy_from_slice(&FORMAT.to_be_bytes());
                bytes[16..24].copy_from_slice(&session.to_be_bytes());
                write_all_at(&self.file, &bytes, SLOT_BASE + u64::from(slot) * SLOT_SIZE)?;
                if slot >= high_water {
                    let mut header = [0; HEADER_SIZE as usize];
                    header[..4].copy_from_slice(&HEADER_MAGIC.to_be_bytes());
                    header[4..8].copy_from_slice(&FORMAT.to_be_bytes());
                    header[8..12].copy_from_slice(&(slot + 1).to_be_bytes());
                    write_all_at(&self.file, &header, HEADER_BASE)?;
                }
                Ok::<_, std::io::Error>(())
            })();
            if let Err(error) = stored {
                let _ = self.apply_byte_mode(lease, Some(true), None);
                return Err(SQLError::Internal(format!(
                    "publish temporary role dependency: {error}"
                )));
            }
            local.by_key.insert((session, role), slot);
            local.by_slot.insert(slot);
            local.next = (slot + 1) % SLOT_COUNT;
            return Ok(());
        }
        Err(super::super::super::temporary_roles::reference_limit_error())
    }

    pub(in crate::row_locks) fn release_temporary_role(&self, session: u64, role: u32) {
        let mut local = self.temporary_role_slots.lock();
        if let Some(slot) = local.by_key.remove(&(session, role)) {
            local.by_slot.remove(&slot);
            // The slot is reusable only after its native lease is gone; a failed unlock remains conservative until process exit.
            let _ = self.apply_byte_mode(LEASE_BASE + u64::from(slot), Some(true), None);
        }
    }

    pub(in crate::row_locks) fn foreign_temporary_role_reference(
        &self,
        role: u32,
        cancel: &CancellationToken,
    ) -> Result<bool, SQLError> {
        let local = self.temporary_role_slots.lock();
        let _admission = self.temporary_role_admission(cancel)?;
        for slot in 0..self.temporary_role_high_water()? {
            cancel.check()?;
            if local.by_slot.contains(&slot) {
                continue;
            }
            let lease = LEASE_BASE + u64::from(slot);
            match self.apply_byte_mode(lease, None, Some(true)) {
                Ok(()) => {
                    self.apply_byte_mode(lease, Some(true), None)
                        .map_err(|error| {
                            SQLError::Internal(format!(
                                "release temporary role dependency probe: {error}"
                            ))
                        })?;
                    continue;
                }
                Err(error) if lock_would_block(&error) => {}
                Err(error) => {
                    return Err(SQLError::Internal(format!(
                        "probe temporary role dependency lease: {error}"
                    )))
                }
            }
            let mut bytes = [0; SLOT_SIZE as usize];
            read_exact_at(
                &self.file,
                &mut bytes,
                SLOT_BASE + u64::from(slot) * SLOT_SIZE,
            )
            .map_err(|error| {
                SQLError::Internal(format!("read temporary role dependency: {error}"))
            })?;
            let word = |offset| {
                u32::from_be_bytes(
                    bytes[offset..offset + 4]
                        .try_into()
                        .expect("dependency word"),
                )
            };
            if word(0) != SLOT_MAGIC
                || word(12) != FORMAT
                || word(4) == 0
                || word(8) == 0
                || bytes[16..24] == [0; 8]
                || bytes[24..] != [0; 8]
            {
                return Err(SQLError::Internal(
                    "invalid live temporary role dependency".into(),
                ));
            }
            if word(4) == role {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
