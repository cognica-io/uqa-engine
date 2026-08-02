//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recovery scan for committed compressed-container records.

use super::{
    chunk_count_for, invalid_data, parse_entry, usize_to_u64, validate_chunk_entry,
    validate_commit_entry, verify_commit_authentication, AuthenticatedChunkRecord, BTreeMap,
    ChunkEntry, ContainerKeys, File, Header, Read, Seek, SeekFrom, AEAD_TAG_LEN, AUTH_TAG_LEN,
    CHUNK_COMMIT, COMMIT_CHUNK_ID, ENTRY_SIZE, FLAG_ENCRYPTED, HEADER_SIZE,
};

pub(super) struct CommittedState {
    pub(super) logical_len: usize,
    pub(super) end_offset: u64,
    pub(super) chunks: BTreeMap<u64, ChunkEntry>,
    pub(super) generation: u64,
    pub(super) state_tag: [u8; AUTH_TAG_LEN],
}

pub(super) fn scan_committed_records(
    file: &mut File,
    header: &Header,
    keys: Option<&ContainerKeys>,
    chunk_size: usize,
) -> std::io::Result<CommittedState> {
    RecordScanner::new(file, header, keys, chunk_size)?.scan()
}

struct RecordScanner<'a> {
    file: &'a mut File,
    header: &'a Header,
    keys: Option<&'a ContainerKeys>,
    chunk_size: usize,
    file_len: u64,
    record_offset: u64,
    committed: CommittedState,
    committed_chunk_count: usize,
    pending_generation: Option<u64>,
    pending_records: Vec<AuthenticatedChunkRecord>,
}

impl<'a> RecordScanner<'a> {
    fn new(
        file: &'a mut File,
        header: &'a Header,
        keys: Option<&'a ContainerKeys>,
        chunk_size: usize,
    ) -> std::io::Result<Self> {
        let record_offset = usize_to_u64(HEADER_SIZE, "container header size")?;
        Ok(Self {
            file_len: file.metadata()?.len(),
            file,
            header,
            keys,
            chunk_size,
            record_offset,
            committed: CommittedState {
                logical_len: 0,
                end_offset: record_offset,
                chunks: BTreeMap::new(),
                generation: 0,
                state_tag: [0_u8; AUTH_TAG_LEN],
            },
            committed_chunk_count: 0,
            pending_generation: None,
            pending_records: Vec::new(),
        })
    }

    fn scan(mut self) -> std::io::Result<CommittedState> {
        while let Some((entry, entry_end)) = self.read_entry()? {
            let complete = if entry.flags & CHUNK_COMMIT != 0 {
                self.apply_commit(&entry, entry_end)?
            } else {
                self.apply_chunk(entry, entry_end)?
            };
            if !complete {
                break;
            }
        }
        self.validate_header_state()?;
        Ok(self.committed)
    }

    fn read_entry(&mut self) -> std::io::Result<Option<(ChunkEntry, u64)>> {
        let entry_end = self
            .record_offset
            .checked_add(usize_to_u64(ENTRY_SIZE, "container entry size")?)
            .ok_or_else(|| invalid_data("container entry offset overflow"))?;
        if entry_end > self.file_len {
            return Ok(None);
        }
        self.file.seek(SeekFrom::Start(self.record_offset))?;
        let mut entry_bytes = [0_u8; ENTRY_SIZE];
        self.file.read_exact(&mut entry_bytes)?;
        Ok(Some((parse_entry(&entry_bytes)?, entry_end)))
    }

    fn apply_commit(&mut self, entry: &ChunkEntry, entry_end: u64) -> std::io::Result<bool> {
        if entry.chunk_id != COMMIT_CHUNK_ID {
            return Err(invalid_data("invalid compressed container commit record"));
        }
        let encrypted = self.header.flags & FLAG_ENCRYPTED != 0;
        validate_commit_entry(entry, encrypted)?;
        let tag_end = entry_end
            .checked_add(usize_to_u64(
                entry.allocated_len,
                "commit authentication tag length",
            )?)
            .ok_or_else(|| invalid_data("container commit tag offset overflow"))?;
        if tag_end > self.file_len {
            return Ok(false);
        }
        let state_tag = self.verify_commit(entry, encrypted)?;
        self.validate_commit_generation(entry)?;
        self.publish_commit(entry, state_tag)?;
        self.record_offset = tag_end;
        self.committed.end_offset = tag_end;
        Ok(true)
    }

    fn verify_commit(
        &mut self,
        entry: &ChunkEntry,
        encrypted: bool,
    ) -> std::io::Result<[u8; AUTH_TAG_LEN]> {
        if !encrypted {
            return Ok([0_u8; AUTH_TAG_LEN]);
        }
        let mut tag = [0_u8; AUTH_TAG_LEN];
        self.file.read_exact(&mut tag)?;
        let mac_key = &self
            .keys
            .ok_or_else(|| invalid_data("encrypted commit without key"))?
            .mac_key;
        verify_commit_authentication(
            mac_key,
            &self.header.file_id,
            self.record_offset,
            entry,
            &self.committed.state_tag,
            &self.pending_records,
            &tag,
        )?;
        Ok(tag)
    }

    fn validate_commit_generation(&self, entry: &ChunkEntry) -> std::io::Result<()> {
        if entry.generation <= self.committed.generation {
            return Err(invalid_data(
                "compressed container commit generation did not advance",
            ));
        }
        if self
            .pending_generation
            .is_some_and(|generation| generation != entry.generation)
        {
            return Err(invalid_data(
                "compressed container commit generation does not match pending chunks",
            ));
        }
        Ok(())
    }

    fn publish_commit(
        &mut self,
        entry: &ChunkEntry,
        state_tag: [u8; AUTH_TAG_LEN],
    ) -> std::io::Result<()> {
        let logical_len =
            usize::try_from(entry.offset).map_err(|_| invalid_data("commit logical length"))?;
        let chunk_count = entry.raw_len;
        if chunk_count != chunk_count_for(logical_len, self.chunk_size) {
            return Err(invalid_data(
                "compressed container commit chunk count mismatch",
            ));
        }
        for pending in self.pending_records.drain(..) {
            self.committed
                .chunks
                .insert(pending.entry.chunk_id, pending.entry);
        }
        let active_chunk_count = usize_to_u64(chunk_count, "committed chunk count")?;
        self.committed
            .chunks
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.pending_generation = None;
        self.committed.generation = entry.generation;
        self.committed.state_tag = state_tag;
        self.committed.logical_len = logical_len;
        self.committed_chunk_count = chunk_count;
        Ok(())
    }

    fn apply_chunk(&mut self, entry: ChunkEntry, entry_end: u64) -> std::io::Result<bool> {
        let encrypted = self.header.flags & FLAG_ENCRYPTED != 0;
        validate_chunk_entry(&entry, self.chunk_size, encrypted)?;
        let payload_end = entry_end
            .checked_add(usize_to_u64(entry.allocated_len, "chunk allocated length")?)
            .ok_or_else(|| invalid_data("chunk payload offset overflow"))?;
        if payload_end > self.file_len {
            return Ok(false);
        }
        if entry.offset != entry_end {
            return Err(invalid_data("chunk payload offset mismatch"));
        }
        if entry.generation <= self.committed.generation {
            return Err(invalid_data(
                "compressed container chunk generation was already committed",
            ));
        }
        self.record_pending_generation(entry.generation)?;
        let payload_tag = if encrypted {
            let tag_offset = payload_end
                .checked_sub(usize_to_u64(
                    AEAD_TAG_LEN,
                    "chunk authentication tag length",
                )?)
                .ok_or_else(|| invalid_data("chunk authentication tag offset underflow"))?;
            self.file.seek(SeekFrom::Start(tag_offset))?;
            let mut tag = [0_u8; AEAD_TAG_LEN];
            self.file.read_exact(&mut tag)?;
            tag
        } else {
            [0_u8; AEAD_TAG_LEN]
        };
        self.pending_records
            .push(AuthenticatedChunkRecord { entry, payload_tag });
        self.record_offset = payload_end;
        Ok(true)
    }

    fn record_pending_generation(&mut self, generation: u64) -> std::io::Result<()> {
        match self.pending_generation {
            Some(pending) if pending != generation => Err(invalid_data(
                "compressed container has interleaved uncommitted generations",
            )),
            None => {
                self.pending_generation = Some(generation);
                Ok(())
            }
            Some(_) => Ok(()),
        }
    }

    fn validate_header_state(&self) -> std::io::Result<()> {
        if self.committed.generation < self.header.generation {
            return Err(invalid_data(
                "compressed container was truncated or rolled back before its authenticated header generation",
            ));
        }
        if self.committed.generation == self.header.generation
            && (self.committed.logical_len != self.header.logical_len
                || self.committed_chunk_count != self.header.chunk_count)
        {
            return Err(invalid_data(
                "compressed container header does not match its authenticated commit",
            ));
        }
        Ok(())
    }
}
