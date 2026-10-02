//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Layout of the row claim sidecar: a header, one record for each attached process, and an open-addressing table of claims.
//!
//! Every read and write happens under the claim table lock byte. A probe never wraps: it ends at an empty slot or at the end of a margin after the last home slot. A rebuild writes its entries to a journal after the table before it touches a slot, and the header names that journal until the new table is complete, so a process that dies while rebuilding leaves a table the next holder of the lock byte finishes.

use std::fs::File;
use std::io::{Error, ErrorKind, Result};

use uqa_storage::native_file::read_zero_extended_at;

use super::super::{read_exact_at, write_all_at};

pub(super) const PROCESS_SLOT_COUNT: u16 = 4096;
const PROCESS_SLOT_BASE: u64 = 64;
const PROCESS_SLOT_SIZE: u64 = 8;
const TABLE_BASE: u64 = 64 * 1024;
const ENTRY_SIZE: u64 = 32;
pub(super) const INITIAL_CAPACITY_LOG2: u32 = 12;
const MAXIMUM_CAPACITY_LOG2: u32 = 40;
/// Slots after the last home slot, which end a run of claims near the end of the table without wrapping.
const MARGIN: u64 = 256;
/// Slots read by one positioned read of a probe.
const WINDOW: u64 = 16;
/// Slots written by one positioned write of a rebuild.
const CHUNK: u64 = 32 * 1024;
const HEADER_SIZE: usize = 64;
const HEADER_MAGIC: u32 = 0x5551_5243;
const HEADER_VERSION: u32 = 1;
const STATE_EMPTY: u8 = 0;
const STATE_LIVE: u8 = 1;
const STATE_TOMBSTONE: u8 = 2;

/// One attachment of a process. A slot's generation advances with every attachment, so an owner is never named twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Owner {
    pub(super) slot: u16,
    pub(super) generation: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Mode {
    #[default]
    None,
    Shared,
    Exclusive,
}

impl Mode {
    fn decode(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(Self::None),
            1 => Ok(Self::Shared),
            2 => Ok(Self::Exclusive),
            _ => Err(invalid("row claim mode")),
        }
    }
}

/// The claims one session of one process holds on the two bytes of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) identity: u64,
    pub(super) session: u64,
    pub(super) owner: Owner,
    pub(super) key: Mode,
    pub(super) row: Mode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Slot {
    Empty,
    /// A removed claim that still separates a later claim from its home slot.
    Tombstone,
    Live(Entry),
}

impl Slot {
    fn decode(bytes: &[u8]) -> Result<Self> {
        match bytes[22] {
            STATE_EMPTY => Ok(Self::Empty),
            STATE_TOMBSTONE => Ok(Self::Tombstone),
            STATE_LIVE => {
                let key = Mode::decode(bytes[23])?;
                let row = Mode::decode(bytes[24])?;
                if key == Mode::None && row == Mode::None {
                    return Err(invalid("row claim without a mode"));
                }
                Ok(Self::Live(Entry {
                    identity: u64::from_be_bytes(bytes[0..8].try_into().expect("identity")),
                    session: u64::from_be_bytes(bytes[8..16].try_into().expect("session")),
                    owner: Owner {
                        generation: u32::from_be_bytes(
                            bytes[16..20].try_into().expect("generation"),
                        ),
                        slot: u16::from_be_bytes(bytes[20..22].try_into().expect("slot")),
                    },
                    key,
                    row,
                }))
            }
            _ => Err(invalid("row claim state")),
        }
    }

    fn encode(&self) -> [u8; ENTRY_SIZE as usize] {
        let mut bytes = [0_u8; ENTRY_SIZE as usize];
        match self {
            Self::Empty => {}
            Self::Tombstone => bytes[22] = STATE_TOMBSTONE,
            Self::Live(entry) => {
                bytes[0..8].copy_from_slice(&entry.identity.to_be_bytes());
                bytes[8..16].copy_from_slice(&entry.session.to_be_bytes());
                bytes[16..20].copy_from_slice(&entry.owner.generation.to_be_bytes());
                bytes[20..22].copy_from_slice(&entry.owner.slot.to_be_bytes());
                bytes[22] = STATE_LIVE;
                bytes[23] = entry.key as u8;
                bytes[24] = entry.row as u8;
            }
        }
        bytes
    }
}

/// A rebuild whose journal is complete and whose table is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rebuild {
    journal: u64,
    entries: u64,
    capacity_log2: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) capacity_log2: u32,
    /// Advances whenever the table is reset or rebuilt.
    pub(super) epoch: u64,
    pub(super) rebuild: Option<Rebuild>,
}

impl Header {
    fn decode(bytes: &[u8; HEADER_SIZE]) -> Result<Self> {
        let word =
            |offset: usize| u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("word"));
        let long =
            |offset: usize| u64::from_be_bytes(bytes[offset..offset + 8].try_into().expect("long"));
        let capacity = |log2: u32| (INITIAL_CAPACITY_LOG2..=MAXIMUM_CAPACITY_LOG2).contains(&log2);
        if word(0) != HEADER_MAGIC || word(4) != HEADER_VERSION || !capacity(word(12)) {
            return Err(invalid("row claim table header"));
        }
        let rebuild = match word(8) {
            0 => None,
            1 if capacity(word(40)) => Some(Rebuild {
                journal: long(24),
                entries: long(32),
                capacity_log2: word(40),
            }),
            _ => return Err(invalid("row claim table state")),
        };
        Ok(Self {
            capacity_log2: word(12),
            epoch: long(16),
            rebuild,
        })
    }

    fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut bytes = [0_u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&HEADER_MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&HEADER_VERSION.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.capacity_log2.to_be_bytes());
        bytes[16..24].copy_from_slice(&self.epoch.to_be_bytes());
        if let Some(rebuild) = self.rebuild {
            bytes[8..12].copy_from_slice(&1_u32.to_be_bytes());
            bytes[24..32].copy_from_slice(&rebuild.journal.to_be_bytes());
            bytes[32..40].copy_from_slice(&rebuild.entries.to_be_bytes());
            bytes[40..44].copy_from_slice(&rebuild.capacity_log2.to_be_bytes());
        }
        bytes
    }

    pub(super) fn capacity(&self) -> u64 {
        1 << self.capacity_log2
    }
}

fn invalid(what: &'static str) -> Error {
    Error::new(ErrorKind::InvalidData, format!("invalid {what}"))
}

fn slots(capacity_log2: u32) -> u64 {
    (1 << capacity_log2) + MARGIN
}

fn home(identity: u64, capacity_log2: u32) -> u64 {
    identity.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - capacity_log2)
}

fn slot_offset(index: u64) -> u64 {
    TABLE_BASE + index * ENTRY_SIZE
}

/// The header, or `None` for a sidecar that was never initialized.
pub(super) fn read_header(file: &File) -> Result<Option<Header>> {
    let mut bytes = [0_u8; HEADER_SIZE];
    read_zero_extended_at(file, &mut bytes, 0)?;
    if bytes == [0_u8; HEADER_SIZE] {
        return Ok(None);
    }
    Header::decode(&bytes).map(Some)
}

fn write_header(file: &File, header: &Header) -> Result<()> {
    write_all_at(file, &header.encode(), 0)
}

/// The generation and process ID recorded for every process slot.
pub(super) fn read_processes(file: &File) -> Result<Vec<(u32, u32)>> {
    let mut bytes = vec![0_u8; usize::from(PROCESS_SLOT_COUNT) * PROCESS_SLOT_SIZE as usize];
    read_zero_extended_at(file, &mut bytes, PROCESS_SLOT_BASE)?;
    Ok(bytes
        .chunks_exact(PROCESS_SLOT_SIZE as usize)
        .map(|record| {
            (
                u32::from_be_bytes(record[0..4].try_into().expect("generation")),
                u32::from_be_bytes(record[4..8].try_into().expect("process")),
            )
        })
        .collect())
}

pub(super) fn read_process(file: &File, slot: u16) -> Result<(u32, u32)> {
    let mut record = [0_u8; PROCESS_SLOT_SIZE as usize];
    read_zero_extended_at(
        file,
        &mut record,
        PROCESS_SLOT_BASE + u64::from(slot) * PROCESS_SLOT_SIZE,
    )?;
    Ok((
        u32::from_be_bytes(record[0..4].try_into().expect("generation")),
        u32::from_be_bytes(record[4..8].try_into().expect("process")),
    ))
}

pub(super) fn write_process(file: &File, slot: u16, generation: u32, process: u32) -> Result<()> {
    let mut record = [0_u8; PROCESS_SLOT_SIZE as usize];
    record[0..4].copy_from_slice(&generation.to_be_bytes());
    record[4..8].copy_from_slice(&process.to_be_bytes());
    write_all_at(
        file,
        &record,
        PROCESS_SLOT_BASE + u64::from(slot) * PROCESS_SLOT_SIZE,
    )
}

/// Replace the table with an empty one of the initial capacity, keeping the process records.
pub(super) fn initialize(file: &File, previous: Option<Header>) -> Result<Header> {
    let header = Header {
        capacity_log2: INITIAL_CAPACITY_LOG2,
        epoch: previous.map_or(1, |header| header.epoch.wrapping_add(1)),
        rebuild: None,
    };
    // Shortening first makes the extension read as zero, which is an empty slot.
    file.set_len(TABLE_BASE)?;
    file.set_len(slot_offset(slots(header.capacity_log2)))?;
    write_header(file, &header)?;
    Ok(header)
}

/// Where a probe stopped and how many occupied slots it passed.
pub(super) struct Probe {
    /// The empty slot that ended the run, or `None` when the run reaches the end of the table.
    pub(super) empty: Option<u64>,
    pub(super) length: u64,
}

/// The claim table of one header. The caller holds the claim table lock byte.
pub(super) struct Table<'a> {
    file: &'a File,
    capacity_log2: u32,
}

impl<'a> Table<'a> {
    pub(super) fn new(file: &'a File, header: &Header) -> Self {
        Self {
            file,
            capacity_log2: header.capacity_log2,
        }
    }

    /// Read at most a window of slots from `first` into `bytes` and return how many were read. Every slot of the table lies inside the file.
    fn read_window(
        &self,
        first: u64,
        count: u64,
        bytes: &mut [u8; (WINDOW * ENTRY_SIZE) as usize],
    ) -> Result<usize> {
        let count = WINDOW.min(count) as usize;
        read_exact_at(
            self.file,
            &mut bytes[..count * ENTRY_SIZE as usize],
            slot_offset(first),
        )?;
        Ok(count)
    }

    fn read_slot(&self, index: u64) -> Result<Slot> {
        let mut bytes = [0_u8; ENTRY_SIZE as usize];
        read_exact_at(self.file, &mut bytes, slot_offset(index))?;
        Slot::decode(&bytes)
    }

    pub(super) fn write(&self, index: u64, slot: &Slot) -> Result<()> {
        write_all_at(self.file, &slot.encode(), slot_offset(index))
    }

    /// Visit every occupied slot of the run that starts at the home slot of `identity`. Every claim of `identity` is in that run.
    pub(super) fn probe(
        &self,
        identity: u64,
        visit: &mut dyn FnMut(u64, Slot) -> Result<()>,
    ) -> Result<Probe> {
        let end = slots(self.capacity_log2);
        let mut index = home(identity, self.capacity_log2);
        let mut length = 0;
        let mut bytes = [0_u8; (WINDOW * ENTRY_SIZE) as usize];
        while index < end {
            let count = self.read_window(index, end - index, &mut bytes)?;
            for slot in bytes.chunks_exact(ENTRY_SIZE as usize).take(count) {
                let slot = Slot::decode(slot)?;
                if slot == Slot::Empty {
                    return Ok(Probe {
                        empty: Some(index),
                        length,
                    });
                }
                visit(index, slot)?;
                length += 1;
                index += 1;
            }
        }
        Ok(Probe {
            empty: None,
            length,
        })
    }

    /// Remove the claim at `index`. A claim that ends its run empties its slot and the tombstones before it, which no longer separate any claim from its home slot; any other claim leaves a tombstone.
    pub(super) fn remove(&self, index: u64) -> Result<()> {
        let end = slots(self.capacity_log2);
        if index + 1 < end && self.read_slot(index + 1)? != Slot::Empty {
            return self.write(index, &Slot::Tombstone);
        }
        let mut first = index;
        let mut bytes = [0_u8; (WINDOW * ENTRY_SIZE) as usize];
        'tombstones: while first > 0 {
            let count = self.read_window(first - WINDOW.min(first), first, &mut bytes)?;
            for slot in bytes.chunks_exact(ENTRY_SIZE as usize).take(count).rev() {
                if Slot::decode(slot)? != Slot::Tombstone {
                    break 'tombstones;
                }
                first -= 1;
            }
        }
        let zeros = vec![0_u8; ((index - first + 1) * ENTRY_SIZE) as usize];
        write_all_at(self.file, &zeros, slot_offset(first))
    }

    /// Visit every claim of the table.
    pub(super) fn scan(&self, visit: &mut dyn FnMut(Entry) -> Result<()>) -> Result<()> {
        let end = slots(self.capacity_log2);
        let mut index = 0;
        let mut bytes = Vec::new();
        while index < end {
            let count = CHUNK.min(end - index);
            bytes.resize((count * ENTRY_SIZE) as usize, 0);
            read_exact_at(self.file, &mut bytes, slot_offset(index))?;
            for slot in bytes.chunks_exact(ENTRY_SIZE as usize) {
                if let Slot::Live(entry) = Slot::decode(slot)? {
                    visit(entry)?;
                }
            }
            index += count;
        }
        Ok(())
    }
}

/// The slot each of `entries`, ordered by home slot, takes in a table of `capacity_log2`, or `None` when a run passes the end of the table.
fn placements(entries: &[Entry], capacity_log2: u32) -> Option<Vec<u64>> {
    let end = slots(capacity_log2);
    let mut next = 0;
    entries
        .iter()
        .map(|entry| {
            let position = home(entry.identity, capacity_log2).max(next);
            next = position + 1;
            (position < end).then_some(position)
        })
        .collect()
}

fn order(entries: &mut [Entry], capacity_log2: u32) {
    entries.sort_unstable_by_key(|entry| {
        (
            home(entry.identity, capacity_log2),
            entry.identity,
            entry.owner.slot,
            entry.session,
        )
    });
}

/// Replace the table with one that holds exactly `entries` and has room for `additional` more at no more than a quarter full.
pub(super) fn rebuild(
    file: &File,
    header: &mut Header,
    mut entries: Vec<Entry>,
    additional: u64,
) -> Result<()> {
    journal(file, header, &mut entries, additional)?;
    apply(file, header, &mut entries)
}

/// Write the journal of a rebuild and name it in the header. The table itself is untouched.
fn journal(file: &File, header: &mut Header, entries: &mut [Entry], additional: u64) -> Result<()> {
    let wanted = (entries.len() as u64)
        .saturating_add(additional)
        .saturating_mul(4);
    let mut capacity_log2 = INITIAL_CAPACITY_LOG2;
    while (1_u64 << capacity_log2) < wanted {
        capacity_log2 += 1;
    }
    loop {
        if capacity_log2 > MAXIMUM_CAPACITY_LOG2 {
            return Err(Error::other("row claim table exceeds its largest capacity"));
        }
        order(entries, capacity_log2);
        if placements(entries, capacity_log2).is_some() {
            break;
        }
        capacity_log2 += 1;
    }
    let journal = slot_offset(slots(header.capacity_log2).max(slots(capacity_log2)));
    let mut offset = journal;
    for chunk in entries.chunks(CHUNK as usize) {
        let mut bytes = Vec::with_capacity(chunk.len() * ENTRY_SIZE as usize);
        for entry in chunk {
            bytes.extend_from_slice(&Slot::Live(*entry).encode());
        }
        write_all_at(file, &bytes, offset)?;
        offset += bytes.len() as u64;
    }
    header.epoch = header.epoch.wrapping_add(1);
    header.rebuild = Some(Rebuild {
        journal,
        entries: entries.len() as u64,
        capacity_log2,
    });
    write_header(file, header)
}

/// Finish the rebuild a dead process left behind.
pub(super) fn recover(file: &File, header: &mut Header) -> Result<()> {
    let Some(rebuild) = header.rebuild else {
        return Ok(());
    };
    let mut entries = Vec::new();
    let mut read = 0;
    while read < rebuild.entries {
        let count = CHUNK.min(rebuild.entries - read);
        let mut bytes = vec![0_u8; (count * ENTRY_SIZE) as usize];
        read_exact_at(file, &mut bytes, rebuild.journal + read * ENTRY_SIZE)?;
        for slot in bytes.chunks_exact(ENTRY_SIZE as usize) {
            let Slot::Live(entry) = Slot::decode(slot)? else {
                return Err(invalid("row claim rebuild journal"));
            };
            entries.push(entry);
        }
        read += count;
    }
    apply(file, header, &mut entries)
}

/// Write the table of the rebuild `header` names from `entries`, then publish it.
fn apply(file: &File, header: &mut Header, entries: &mut [Entry]) -> Result<()> {
    let rebuild = header
        .rebuild
        .ok_or_else(|| invalid("row claim rebuild state"))?;
    order(entries, rebuild.capacity_log2);
    let positions = placements(entries, rebuild.capacity_log2)
        .ok_or_else(|| invalid("row claim rebuild capacity"))?;
    let end = slots(rebuild.capacity_log2);
    let mut placed = 0;
    let mut first = 0;
    while first < end {
        let count = CHUNK.min(end - first);
        let mut bytes = vec![0_u8; (count * ENTRY_SIZE) as usize];
        while placed < entries.len() && positions[placed] < first + count {
            let at = ((positions[placed] - first) * ENTRY_SIZE) as usize;
            bytes[at..at + ENTRY_SIZE as usize]
                .copy_from_slice(&Slot::Live(entries[placed]).encode());
            placed += 1;
        }
        write_all_at(file, &bytes, slot_offset(first))?;
        first += count;
    }
    header.capacity_log2 = rebuild.capacity_log2;
    header.rebuild = None;
    write_header(file, header)?;
    file.set_len(slot_offset(end))
}

#[cfg(test)]
mod tests;
