//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent binary Patricia roots have at most 128 branch steps for a u128 key.

use super::{invalid, io, Record};
use crate::{
    read_control::StorageReadControl, temporary_file::BlockTemporaryFile, StorageBackendResult,
};
use parking_lot::Mutex;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::Arc,
};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError, MemoryReservation};

const HEADER_BYTES: usize = 64;
const LEAF: u8 = 128;
const BLOCK_BYTES: usize = 1024;

#[derive(Clone, Copy, Default)]
struct Header {
    bit: u8,
    key: u128,
    left: u64,
    right: u64,
    length: u64,
    memory: u64,
}

#[derive(Clone, Copy, Default)]
struct Cache {
    offset: u64,
    header: Header,
}

struct Pages {
    file: BlockTemporaryFile<BLOCK_BYTES>,
    cache: Vec<Cache>,
    _memory: MemoryReservation,
}

#[derive(Clone)]
pub(super) struct Map {
    file: Arc<Mutex<Pages>>,
    root: u64,
    logical_bytes: u64,
}

/// Ordered input builds each branch once, without retaining obsolete insertion paths in the new file.
pub(super) struct Builder {
    map: Map,
    path: [Header; 128],
    depth: usize,
    previous: Option<u128>,
    _workspace: MemoryReservation,
}

impl Builder {
    pub(super) fn new(memory: &MemoryBudget) -> StorageBackendResult<Self> {
        let workspace = memory.reserve(size_of::<[Header; 128]>())?;
        Ok(Self {
            map: Map::new(memory)?,
            path: [Header::default(); 128],
            depth: 0,
            previous: None,
            _workspace: workspace,
        })
    }

    pub(super) fn push<V: Record>(
        &mut self,
        key: u128,
        value: &V,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<()> {
        let encoded = value.encoded_bytes()?;
        let mut encoded_memory = memory.reserve(encoded)?;
        let mut bytes = Vec::with_capacity(encoded);
        value.encode(&mut bytes);
        if bytes.len() != encoded {
            return Err(invalid("encoded size"));
        }
        encoded_memory.grow(bytes.capacity() - encoded)?;
        let mut file = self.map.file.lock();
        if let Some(previous) = self.previous {
            if key <= previous {
                return Err(invalid("unordered bulk input"));
            }
            let differing = (key ^ previous).leading_zeros() as u8;
            while self.depth > 0 && self.path[self.depth - 1].bit >= differing {
                self.depth -= 1;
                let mut branch = self.path[self.depth];
                branch.right = self.map.root;
                self.map.root = file.append_header(branch)?;
            }
            self.path[self.depth] = Header {
                bit: differing,
                key: previous,
                left: self.map.root,
                ..Header::default()
            };
            self.depth += 1;
            self.map.logical_bytes = self
                .map
                .logical_bytes
                .checked_add(HEADER_BYTES as u64)
                .ok_or(MemoryError::SizeOverflow)?;
        }
        self.map.root = file.append_value(key, value.memory_bytes()?, &bytes)?;
        self.map.logical_bytes = self
            .map
            .logical_bytes
            .checked_add((HEADER_BYTES + encoded) as u64)
            .ok_or(MemoryError::SizeOverflow)?;
        self.previous = Some(key);
        Ok(())
    }

    pub(super) fn finish(mut self) -> StorageBackendResult<Map> {
        {
            let mut file = self.map.file.lock();
            while self.depth > 0 {
                self.depth -= 1;
                let mut branch = self.path[self.depth];
                branch.right = self.map.root;
                self.map.root = file.append_header(branch)?;
            }
        }
        Ok(self.map)
    }
}

impl Map {
    #[cfg(test)]
    pub(super) fn path(&self) -> std::path::PathBuf {
        self.file.lock().file.path().to_path_buf()
    }

    #[cfg(test)]
    pub(super) fn fail_write_after(&self, bytes: usize) {
        self.file.lock().file.fail_write_after(bytes);
    }

    pub(super) fn new(memory: &MemoryBudget) -> StorageBackendResult<Self> {
        let slots = (memory.limit() / 64 / size_of::<Cache>()).min(256);
        let slots = if slots == 0 { 0 } else { 1 << slots.ilog2() };
        let reservation = memory.reserve(size_of::<Pages>() + slots * size_of::<Cache>())?;
        let mut file = BlockTemporaryFile::new().map_err(io)?;
        file.write_all(&[0; HEADER_BYTES]).map_err(io)?;
        Ok(Self {
            file: Arc::new(Mutex::new(Pages {
                file,
                cache: vec![Cache::default(); slots],
                _memory: reservation,
            })),
            root: 0,
            logical_bytes: 0,
        })
    }

    pub(super) fn get<V: Record>(
        &self,
        key: u128,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Option<Budgeted<V>>> {
        if self.root == 0 {
            return Ok(None);
        }
        let mut file = self.file.lock();
        let (_, leaf) = file.leaf(self.root, key)?;
        if leaf.key != key {
            return Ok(None);
        }
        file.value(leaf, memory).map(Some)
    }

    pub(super) fn last<V: Record>(
        &self,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Option<(u128, Budgeted<V>)>> {
        if self.root == 0 {
            return Ok(None);
        }
        let mut file = self.file.lock();
        let mut offset = self.root;
        for _ in 0..=128 {
            let header = file.header(offset)?;
            if header.bit == LEAF {
                return Ok(Some((header.key, file.value(header, memory)?)));
            }
            offset = header.right;
        }
        Err(invalid("Patricia depth"))
    }

    pub(super) fn next<V: Record>(
        &self,
        after: Option<u128>,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Option<(u128, Budgeted<V>)>> {
        if self.root == 0 {
            return Ok(None);
        }
        let _workspace = memory.reserve(size_of::<[u64; 128]>())?;
        let mut file = self.file.lock();
        let mut offset = self.root;
        let mut pending = [0_u64; 128];
        let mut depth = 0;
        loop {
            let header = file.header(offset)?;
            if header.bit == LEAF {
                if after.is_none_or(|key| header.key > key) {
                    return Ok(Some((header.key, file.value(header, memory)?)));
                }
            } else {
                // Every child shares this prefix. Skip a complete subtree if its greatest possible key is already behind the cursor.
                let suffix = u128::MAX >> header.bit;
                if after.is_none_or(|key| (header.key | suffix) > key) {
                    if depth == pending.len() {
                        return Err(invalid("Patricia depth"));
                    }
                    pending[depth] = header.right;
                    depth += 1;
                    offset = header.left;
                    continue;
                }
            }
            let Some(next_depth) = depth.checked_sub(1) else {
                return Ok(None);
            };
            depth = next_depth;
            offset = pending[depth];
        }
    }

    pub(super) fn insert<V: Record>(
        &mut self,
        key: u128,
        value: &V,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<()> {
        let _workspace = memory.reserve(size_of::<[u64; 128]>())?;
        let encoded = value.encoded_bytes()?;
        let mut encoded_memory = memory.reserve(encoded)?;
        let mut bytes = Vec::with_capacity(encoded);
        value.encode(&mut bytes);
        if bytes.len() != encoded {
            return Err(invalid("encoded size"));
        }
        encoded_memory.grow(bytes.capacity() - encoded)?;
        let retained = value.memory_bytes()?;
        let mut file = self.file.lock();
        let original = file.file.metadata().map_err(io)?.len();
        let result = (|| {
            let existing = if self.root == 0 {
                None
            } else {
                Some(file.leaf(self.root, key)?)
            };
            let differing = existing.map_or(0, |(_, leaf)| (key ^ leaf.key).leading_zeros() as u8);
            let mut path = [0_u64; 128];
            let mut depth = 0;
            let mut previous = self.root;
            while previous != 0 {
                let header = file.header(previous)?;
                if header.bit >= differing {
                    break;
                }
                let right = bit(key, header.bit);
                path[depth] = previous;
                depth += 1;
                previous = if right { header.right } else { header.left };
            }
            let mut root = file.append_value(key, retained, &bytes)?;
            let replaced = existing.filter(|(_, leaf)| leaf.key == key);
            if previous != 0 && replaced.is_none() {
                let right = bit(key, differing);
                root = file.append_header(Header {
                    bit: differing,
                    key,
                    left: if right { previous } else { root },
                    right: if right { root } else { previous },
                    ..Header::default()
                })?;
            }
            for offset in path[..depth].iter().rev().copied() {
                let mut header = file.header(offset)?;
                let right = bit(key, header.bit);
                if right {
                    header.right = root;
                } else {
                    header.left = root;
                }
                root = file.append_header(header)?;
            }
            let removed = replaced.map_or(0, |(_, leaf)| leaf.length + HEADER_BYTES as u64);
            let added = bytes.len() as u64
                + HEADER_BYTES as u64
                + u64::from(previous != 0 && replaced.is_none()) * HEADER_BYTES as u64;
            Ok((
                root,
                self.logical_bytes
                    .checked_sub(removed)
                    .and_then(|n| n.checked_add(added))
                    .ok_or(MemoryError::SizeOverflow)?,
            ))
        })();
        match result {
            Ok((root, logical_bytes)) => {
                self.root = root;
                self.logical_bytes = logical_bytes;
                Ok(())
            }
            Err(error) => {
                file.rollback(original)?;
                Err(error)
            }
        }
    }

    pub(super) fn remove(&mut self, key: u128, memory: &MemoryBudget) -> StorageBackendResult<()> {
        if self.root == 0 {
            return Ok(());
        }
        let _workspace = memory.reserve(size_of::<[u64; 128]>())?;
        let mut file = self.file.lock();
        let original = file.file.metadata().map_err(io)?.len();
        let result = (|| {
            let mut path = [0_u64; 128];
            let mut depth = 0;
            let mut offset = self.root;
            let leaf = loop {
                let header = file.header(offset)?;
                if header.bit == LEAF {
                    break header;
                }
                let right = bit(key, header.bit);
                path[depth] = offset;
                depth += 1;
                offset = if right { header.right } else { header.left };
            };
            if leaf.key != key {
                return Ok((self.root, self.logical_bytes));
            }
            let mut root = 0;
            let mut removed = HEADER_BYTES as u64 + leaf.length;
            if depth > 0 {
                depth -= 1;
                let parent = file.header(path[depth])?;
                let right = bit(key, parent.bit);
                root = if right { parent.left } else { parent.right };
                removed += HEADER_BYTES as u64;
                for offset in path[..depth].iter().rev().copied() {
                    let mut header = file.header(offset)?;
                    let right = bit(key, header.bit);
                    if right {
                        header.right = root;
                    } else {
                        header.left = root;
                    }
                    root = file.append_header(header)?;
                }
            }
            Ok((
                root,
                self.logical_bytes
                    .checked_sub(removed)
                    .ok_or_else(|| invalid("live byte count"))?,
            ))
        })();
        match result {
            Ok((root, logical_bytes)) => {
                self.root = root;
                self.logical_bytes = logical_bytes;
                Ok(())
            }
            Err(error) => {
                file.rollback(original)?;
                Err(error)
            }
        }
    }

    pub(super) fn compact<V: Record>(
        &mut self,
        memory: &MemoryBudget,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<()> {
        let bytes = self.file.lock().file.metadata().map_err(io)?.len();
        if bytes <= self.logical_bytes.saturating_mul(4).max(1024 * 1024) {
            return Ok(());
        }
        let mut replacement = Builder::new(memory)?;
        let mut after = None;
        while let Some((key, value)) = self.next::<V>(after, memory)? {
            super::check(control)?;
            replacement.push(key, &*value, memory)?;
            after = Some(key);
        }
        *self = replacement.finish()?;
        Ok(())
    }
}

fn bit(key: u128, position: u8) -> bool {
    key & (1_u128 << (127 - position)) != 0
}

impl Pages {
    fn cache_position(&self, offset: u64) -> Option<usize> {
        if self.cache.is_empty() {
            return None;
        }
        Some(
            ((offset ^ (offset >> 17)).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32) as usize
                & (self.cache.len() - 1),
        )
    }

    fn header(&mut self, offset: u64) -> StorageBackendResult<Header> {
        if let Some(position) = self.cache_position(offset) {
            if self.cache[position].offset == offset {
                return Ok(self.cache[position].header);
            }
        }
        self.file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut bytes = [0; HEADER_BYTES];
        self.file.read_exact(&mut bytes).map_err(io)?;
        let header = Header {
            bit: bytes[0],
            key: u128::from_le_bytes(bytes[1..17].try_into().unwrap()),
            left: u64::from_le_bytes(bytes[17..25].try_into().unwrap()),
            right: u64::from_le_bytes(bytes[25..33].try_into().unwrap()),
            length: u64::from_le_bytes(bytes[33..41].try_into().unwrap()),
            memory: u64::from_le_bytes(bytes[41..49].try_into().unwrap()),
        };
        if header.bit > LEAF || offset == 0 {
            return Err(invalid("tree header"));
        }
        if let Some(position) = self.cache_position(offset) {
            self.cache[position] = Cache { offset, header };
        }
        Ok(header)
    }

    fn leaf(&mut self, mut offset: u64, key: u128) -> StorageBackendResult<(u64, Header)> {
        for _ in 0..=128 {
            let header = self.header(offset)?;
            if header.bit == LEAF {
                return Ok((offset, header));
            }
            offset = if bit(key, header.bit) {
                header.right
            } else {
                header.left
            };
        }
        Err(invalid("Patricia depth"))
    }

    fn value<V: Record>(
        &mut self,
        header: Header,
        memory: &MemoryBudget,
    ) -> StorageBackendResult<Budgeted<V>> {
        let size = usize::try_from(header.length).map_err(|_| MemoryError::SizeOverflow)?;
        let _encoded = memory.reserve(size)?;
        let retained = memory
            .reserve(usize::try_from(header.memory).map_err(|_| MemoryError::SizeOverflow)?)?;
        let mut bytes = vec![0; size];
        self.file.seek(SeekFrom::Start(header.left)).map_err(io)?;
        self.file.read_exact(&mut bytes).map_err(io)?;
        let value = V::decode(&bytes)?;
        if value.memory_bytes()? > retained.bytes() {
            return Err(invalid("decoded size"));
        }
        Ok(Budgeted::new(value, retained))
    }

    fn append_header(&mut self, header: Header) -> StorageBackendResult<u64> {
        let offset = self.file.seek(SeekFrom::End(0)).map_err(io)?;
        let mut bytes = [0; HEADER_BYTES];
        bytes[0] = header.bit;
        bytes[1..17].copy_from_slice(&header.key.to_le_bytes());
        bytes[17..25].copy_from_slice(&header.left.to_le_bytes());
        bytes[25..33].copy_from_slice(&header.right.to_le_bytes());
        bytes[33..41].copy_from_slice(&header.length.to_le_bytes());
        bytes[41..49].copy_from_slice(&header.memory.to_le_bytes());
        self.file.write_all(&bytes).map_err(io)?;
        if let Some(position) = self.cache_position(offset) {
            self.cache[position] = Cache { offset, header };
        }
        Ok(offset)
    }

    fn append_value(
        &mut self,
        key: u128,
        memory: usize,
        bytes: &[u8],
    ) -> StorageBackendResult<u64> {
        let offset = self.file.metadata().map_err(io)?.len();
        let payload = offset
            .checked_add(HEADER_BYTES as u64)
            .ok_or(MemoryError::SizeOverflow)?;
        self.append_header(Header {
            bit: LEAF,
            key,
            left: payload,
            length: bytes.len() as u64,
            memory: memory as u64,
            ..Header::default()
        })?;
        self.file.write_all(bytes).map_err(io)?;
        Ok(offset)
    }

    fn rollback(&mut self, length: u64) -> StorageBackendResult<()> {
        self.cache.fill(Cache::default());
        self.file.set_len(length).map_err(io)
    }
}
