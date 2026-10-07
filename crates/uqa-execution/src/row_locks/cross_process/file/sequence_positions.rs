//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The positions of sequences between their durable records, shared by every process attached to one database.
//!
//! A sequence's durable record holds a value some fetches past the values handed out, as `PostgreSQL` logs a sequence 32 fetches ahead. The exact position lives here, in a sidecar of its own that is written without a sync, as `PostgreSQL` keeps it in a shared buffer. Every value handed out is at or before the record, and a sequence without a position continues after the record, so a lost position leaves a gap and never hands a value out twice.
//!
//! A position survives the failure of a process, because the other processes read the same file. It does not survive a machine failure, which may leave an older position in the file than the values handed out. A process that attaches while no other is attached cannot tell the two failures apart, so it discards every position unless the last process to detach synced them and marked them clean. The first write after that mark removes it, with a sync, before any position changes.
//!
//! A run of the database begins when a process attaches alone. A position kept from an earlier run is marked by the run it was written in; its sequence compares it with the durable record before it trusts it, which discards the positions of a database file that was replaced between two runs.

use uqa_storage::native_file::{lock_byte, read_zero_extended_at, unlock_byte};

use super::super::super::sequence_positions::{
    RecordedSequencePosition, SequencePosition, SequencePositionKey, SequenceSlot,
};
use super::{write_all_at, FileLockCoordinator};

const LOCK_BYTE: u64 = 6;
const HEADER_SIZE: usize = 64;
const SLOT_SIZE: usize = 64;
pub(super) const SLOT_COUNT: u64 = 4096;
/// Slots fetched by one read of a probe.
const WINDOW: u64 = 8;
const MAGIC: u32 = 0x5551_5351;
const VERSION: u32 = 1;
const EMPTY: u8 = 0;
const LIVE: u8 = 1;
const REMOVED: u8 = 2;
/// Used slots at which a sequence that needs a new slot first has the positions of dropped and altered sequences removed.
const CROWDED: u32 = 1024;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    /// Every position was synced by the last process to detach and none changed since.
    clean: bool,
    /// A process died while it wrote the table anew, so its slots are not trusted.
    rebuilding: bool,
    run: u64,
    /// Slots that hold a position or the mark of a removed one.
    used: u32,
    crowded_at: u32,
}

impl Header {
    const fn first() -> Self {
        Self {
            clean: false,
            rebuilding: false,
            run: 1,
            used: 0,
            crowded_at: CROWDED,
        }
    }

    /// The run as slots carry it.
    const fn slot_run(&self) -> u32 {
        self.run as u32
    }
}

fn slot_offset(index: u64) -> u64 {
    HEADER_SIZE as u64 + index * SLOT_SIZE as u64
}

fn home(key: &SequencePositionKey) -> u64 {
    // FNV-1a over both identities, which are random, so any mix spreads them.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in key.object.iter().chain(&key.definition) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash % SLOT_COUNT
}

fn encode(key: &SequencePositionKey, position: &SequencePosition, run: u32) -> [u8; SLOT_SIZE] {
    let mut slot = [0_u8; SLOT_SIZE];
    slot[0..16].copy_from_slice(&key.object);
    slot[16..32].copy_from_slice(&key.definition);
    slot[32..40].copy_from_slice(&position.logged.0.to_be_bytes());
    slot[40..48].copy_from_slice(&position.current.to_be_bytes());
    slot[48..56].copy_from_slice(&position.log_count.to_be_bytes());
    slot[56] = LIVE;
    slot[57] = u8::from(position.logged.1);
    slot[58] = u8::from(position.called);
    slot[60..64].copy_from_slice(&run.to_be_bytes());
    slot
}

fn decode(slot: &[u8]) -> (SequencePositionKey, SequencePosition, u32) {
    let long = |at: usize| i64::from_be_bytes(slot[at..at + 8].try_into().expect("slot long"));
    (
        SequencePositionKey {
            object: slot[0..16].try_into().expect("slot object"),
            definition: slot[16..32].try_into().expect("slot definition"),
        },
        SequencePosition {
            logged: (long(32), slot[57] != 0),
            current: long(40),
            called: slot[58] != 0,
            log_count: long(48),
        },
        u32::from_be_bytes(slot[60..64].try_into().expect("slot run")),
    )
}

fn failed(action: &str, error: &std::io::Error) -> String {
    format!("{action} the cross-process sequence positions: {error}")
}

/// Where a sequence's slot is, and the first slot a new position may take with whether that slot was never used.
struct Probe {
    found: Option<(u64, SequencePosition, u32)>,
    free: Option<(u64, bool)>,
}

impl FileLockCoordinator {
    /// Take the lock of the sequence positions. This process attaches first, which begins a run when no other process is attached.
    pub(in crate::row_locks) fn lock_sequence_positions(&self) -> Result<(), String> {
        self.attach_row_claims_process()?;
        lock_byte(&self.file, LOCK_BYTE, true).map_err(|error| failed("lock", &error))
    }

    pub(in crate::row_locks) fn unlock_sequence_positions(&self) {
        let _ = unlock_byte(&self.file, LOCK_BYTE);
    }

    fn stored_sequence_header(&self) -> std::io::Result<Option<Header>> {
        let mut bytes = [0_u8; HEADER_SIZE];
        read_zero_extended_at(&self.sequence_file, &mut bytes, 0)?;
        let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("word"));
        if word(0) != MAGIC || word(4) != VERSION || bytes[8] > 1 || bytes[9] > 1 {
            return Ok(None);
        }
        Ok(Some(Header {
            clean: bytes[8] == 1,
            rebuilding: bytes[9] == 1,
            run: u64::from_be_bytes(bytes[16..24].try_into().expect("run")),
            used: word(24),
            crowded_at: word(28),
        }))
    }

    /// The header of positions that can be trusted. Positions left by a process that died while it wrote the table anew are discarded.
    fn sequence_header(&self) -> std::io::Result<Option<Header>> {
        match self.stored_sequence_header()? {
            Some(header) if header.rebuilding => {
                self.sequence_file.set_len(0)?;
                Ok(None)
            }
            header => Ok(header),
        }
    }

    fn write_sequence_header(&self, header: &Header) -> std::io::Result<()> {
        let mut bytes = [0_u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&VERSION.to_be_bytes());
        bytes[8] = u8::from(header.clean);
        bytes[9] = u8::from(header.rebuilding);
        bytes[16..24].copy_from_slice(&header.run.to_be_bytes());
        bytes[24..28].copy_from_slice(&header.used.to_be_bytes());
        bytes[28..32].copy_from_slice(&header.crowded_at.to_be_bytes());
        write_all_at(&self.sequence_file, &bytes, 0)
    }

    /// The header a write continues: the stored one without its clean mark, or the first one.
    fn sequence_header_for_write(&self) -> std::io::Result<Header> {
        match self.sequence_header()? {
            Some(header) if header.clean => {
                // The positions change from here on, so they stop being the ones that were synced.
                let header = Header {
                    clean: false,
                    ..header
                };
                self.write_sequence_header(&header)?;
                self.sequence_file.sync_data()?;
                Ok(header)
            }
            Some(header) => Ok(header),
            None => {
                // Whatever the file holds without a header is not a table of positions.
                self.sequence_file.set_len(0)?;
                let header = Header::first();
                self.write_sequence_header(&header)?;
                Ok(header)
            }
        }
    }

    /// Visit the run of slots from a key's home to the first slot that was never used.
    fn probe_sequence_slots(&self, key: &SequencePositionKey) -> std::io::Result<Probe> {
        let mut probe = Probe {
            found: None,
            free: None,
        };
        let mut index = home(key);
        let mut bytes = [0_u8; SLOT_SIZE * WINDOW as usize];
        let mut visited = 0;
        while visited < SLOT_COUNT {
            let count = WINDOW.min(SLOT_COUNT - index).min(SLOT_COUNT - visited);
            let window = &mut bytes[..SLOT_SIZE * count as usize];
            read_zero_extended_at(&self.sequence_file, window, slot_offset(index))?;
            for slot in window.as_chunks::<SLOT_SIZE>().0 {
                match slot[56] {
                    EMPTY => {
                        probe.free.get_or_insert((index, true));
                        return Ok(probe);
                    }
                    LIVE => {
                        let (stored, position, run) = decode(slot);
                        if stored == *key {
                            probe.found = Some((index, position, run));
                            return Ok(probe);
                        }
                    }
                    _ => {
                        probe.free.get_or_insert((index, false));
                    }
                }
                index = (index + 1) % SLOT_COUNT;
                visited += 1;
            }
        }
        Ok(probe)
    }

    fn sequence_slot_state(&self, index: u64) -> std::io::Result<u8> {
        let mut state = [0_u8; 1];
        read_zero_extended_at(&self.sequence_file, &mut state, slot_offset(index) + 56)?;
        Ok(state[0])
    }

    /// The slot of a sequence. The caller holds the positions' lock.
    pub(in crate::row_locks) fn read_sequence_slot(
        &self,
        key: &SequencePositionKey,
    ) -> Result<SequenceSlot, String> {
        let read = || -> std::io::Result<SequenceSlot> {
            let Some(header) = self.sequence_header()? else {
                return Ok(SequenceSlot {
                    recorded: None,
                    recordable: true,
                    crowded: false,
                });
            };
            let probe = self.probe_sequence_slots(key)?;
            Ok(SequenceSlot {
                recorded: probe
                    .found
                    .map(|(_, position, run)| RecordedSequencePosition {
                        position,
                        fresh: run == header.slot_run(),
                    }),
                recordable: probe.found.is_some() || probe.free.is_some(),
                crowded: probe.found.is_none() && header.used >= header.crowded_at,
            })
        };
        read().map_err(|error| failed("read", &error))
    }

    /// Record the position of a sequence and return whether it is recorded; a table without a free slot records nothing. The caller holds the positions' lock.
    pub(in crate::row_locks) fn record_sequence_position(
        &self,
        key: &SequencePositionKey,
        position: &SequencePosition,
    ) -> Result<bool, String> {
        let record = || -> std::io::Result<bool> {
            let mut header = self.sequence_header_for_write()?;
            let probe = self.probe_sequence_slots(key)?;
            let (index, takes_unused) = match (probe.found, probe.free) {
                (Some((index, _, _)), _) => (index, false),
                (None, Some(free)) => free,
                (None, None) => return Ok(false),
            };
            write_all_at(
                &self.sequence_file,
                &encode(key, position, header.slot_run()),
                slot_offset(index),
            )?;
            if takes_unused {
                header.used += 1;
                self.write_sequence_header(&header)?;
            }
            Ok(true)
        };
        record().map_err(|error| failed("write", &error))
    }

    /// Remove the position of a sequence. The caller holds the positions' lock.
    pub(in crate::row_locks) fn remove_sequence_position(
        &self,
        key: &SequencePositionKey,
    ) -> Result<(), String> {
        let remove = || -> std::io::Result<()> {
            if self.sequence_header()?.is_none() {
                return Ok(());
            }
            let Some((index, _, _)) = self.probe_sequence_slots(key)?.found else {
                return Ok(());
            };
            let mut header = self.sequence_header_for_write()?;
            let unused = [0_u8; SLOT_SIZE];
            if self.sequence_slot_state((index + 1) % SLOT_COUNT)? != EMPTY {
                // A later slot may belong to a run that passes through this one.
                let mut removed = unused;
                removed[56] = REMOVED;
                return write_all_at(&self.sequence_file, &removed, slot_offset(index));
            }
            // No run passes through a slot before a never used one, nor through the removed slots before it.
            write_all_at(&self.sequence_file, &unused, slot_offset(index))?;
            let mut freed = 1;
            let mut before = (index + SLOT_COUNT - 1) % SLOT_COUNT;
            while freed < header.used && self.sequence_slot_state(before)? == REMOVED {
                write_all_at(&self.sequence_file, &unused, slot_offset(before))?;
                freed += 1;
                before = (before + SLOT_COUNT - 1) % SLOT_COUNT;
            }
            header.used = header.used.saturating_sub(freed);
            self.write_sequence_header(&header)
        };
        remove().map_err(|error| failed("write", &error))
    }

    fn read_sequence_table(&self) -> std::io::Result<Vec<u8>> {
        let mut bytes = vec![0_u8; SLOT_SIZE * SLOT_COUNT as usize];
        read_zero_extended_at(&self.sequence_file, &mut bytes, slot_offset(0))?;
        Ok(bytes)
    }

    /// Every recorded position. The caller holds the positions' lock.
    pub(in crate::row_locks) fn read_sequence_positions(
        &self,
    ) -> Result<Vec<(SequencePositionKey, RecordedSequencePosition)>, String> {
        let read = || -> std::io::Result<Vec<(SequencePositionKey, RecordedSequencePosition)>> {
            let Some(header) = self.sequence_header()? else {
                return Ok(Vec::new());
            };
            Ok(self
                .read_sequence_table()?
                .as_chunks::<SLOT_SIZE>()
                .0
                .iter()
                .filter(|slot| slot[56] == LIVE)
                .map(|slot| {
                    let (key, position, run) = decode(slot);
                    (
                        key,
                        RecordedSequencePosition {
                            position,
                            fresh: run == header.slot_run(),
                        },
                    )
                })
                .collect())
        };
        read().map_err(|error| failed("read", &error))
    }

    /// Keep the positions `keep` names and write the table anew without the others and without removed slots. The caller holds the positions' lock.
    pub(in crate::row_locks) fn retain_sequence_positions(
        &self,
        keep: &dyn Fn(&SequencePositionKey) -> bool,
    ) -> Result<(), String> {
        let retain = || -> std::io::Result<()> {
            if self.sequence_header()?.is_none() {
                return Ok(());
            }
            let mut header = self.sequence_header_for_write()?;
            let stored = self.read_sequence_table()?;
            let mut table = vec![0_u8; stored.len()];
            let mut kept = 0_u32;
            for slot in stored.as_chunks::<SLOT_SIZE>().0 {
                if slot[56] != LIVE || !keep(&decode(slot).0) {
                    continue;
                }
                let mut index = home(&decode(slot).0) as usize;
                while table[index * SLOT_SIZE + 56] != EMPTY {
                    index = (index + 1) % SLOT_COUNT as usize;
                }
                table[index * SLOT_SIZE..(index + 1) * SLOT_SIZE].copy_from_slice(slot);
                kept += 1;
            }
            // A process that dies between these writes leaves a table no one may trust.
            header.rebuilding = true;
            self.write_sequence_header(&header)?;
            write_all_at(&self.sequence_file, &table, slot_offset(0))?;
            header.rebuilding = false;
            header.used = kept;
            // A table mostly taken by sequences that exist gains nothing from another pass; the sequences it has no room for keep their records exact.
            header.crowded_at = if u64::from(kept) * 2 > SLOT_COUNT {
                u32::MAX
            } else {
                CROWDED.max(kept * 2)
            };
            self.write_sequence_header(&header)
        };
        retain().map_err(|error| failed("rebuild", &error))
    }

    /// Begin a run: no other process is attached, so the positions are either the ones the last process synced and marked clean, which a sequence confirms against its record before it trusts them, or possibly older than the values handed out before a machine failure, and discarded.
    pub(super) fn begin_sequence_position_run(&self) -> std::io::Result<()> {
        match self.stored_sequence_header()? {
            Some(header) if header.clean && !header.rebuilding => {
                self.write_sequence_header(&Header {
                    run: header.run.wrapping_add(1),
                    ..header
                })
            }
            _ => self.sequence_file.set_len(0),
        }
    }

    /// Sync the positions and mark them clean, so that an orderly restart continues every sequence exactly. The caller is the last attached process and holds the claim table's lock, which keeps another from attaching meanwhile.
    pub(super) fn mark_sequence_positions_clean(&self) -> std::io::Result<()> {
        let Some(header) = self.stored_sequence_header()? else {
            return Ok(());
        };
        if header.clean || header.rebuilding {
            return Ok(());
        }
        self.sequence_file.sync_data()?;
        self.write_sequence_header(&Header {
            clean: true,
            ..header
        })?;
        self.sequence_file.sync_data()
    }
}
