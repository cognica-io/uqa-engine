//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authenticated stale-record compaction for compressed containers.

use super::{
    build_commit_entry, build_entry, build_header, chunk_payload_tag, commit_authentication_tag,
    fs, invalid_data, sync_parent_directory, usize_to_u64, AuthenticatedChunkRecord, BTreeMap,
    ChunkEntry, ContainerFile, File, Path, PathBuf, Write, AEAD_TAG_LEN, AUTH_TAG_LEN, ENTRY_SIZE,
    FLAG_ENCRYPTED, HEADER_SIZE, MAX_COMPACT_STALE_BYTES, MIN_COMPACT_STALE_BYTES,
};

struct CompactedState {
    append_offset: u64,
    chunks: BTreeMap<u64, ChunkEntry>,
    state_tag: [u8; AUTH_TAG_LEN],
}

impl ContainerFile {
    pub(super) fn compact_if_needed(&mut self) -> std::io::Result<()> {
        if !self.needs_compaction()? {
            return Ok(());
        }
        let next_generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("container generation overflow during compaction"))?;
        let tmp_path = self.compaction_path();
        let compacted = self.write_compacted_file(&tmp_path, next_generation)?;
        fs::rename(&tmp_path, &self.path)?;
        // The rename has already replaced the container in the namespace, so
        // adopt the compacted state before surfacing any directory-sync
        // failure; every return path must leave this handle describing the
        // file that is now on disk.
        self.generation = next_generation;
        self.state_tag = compacted.state_tag;
        self.append_offset = compacted.append_offset;
        self.committed_file_len = compacted.append_offset;
        self.chunks = compacted.chunks;
        sync_parent_directory(&self.path)
    }

    fn needs_compaction(&self) -> std::io::Result<bool> {
        let active_record_bytes = self.active_record_bytes()?;
        let commit_authentication_bytes = if self.keys.is_some() {
            usize_to_u64(AUTH_TAG_LEN, "commit authentication tag length")?
        } else {
            0
        };
        let compact_len = usize_to_u64(HEADER_SIZE + ENTRY_SIZE, "fixed compacted bytes")?
            .checked_add(active_record_bytes)
            .and_then(|length| length.checked_add(commit_authentication_bytes))
            .ok_or_else(|| invalid_data("compacted container length overflow"))?;
        let stale_bytes = self.append_offset.saturating_sub(compact_len);
        let threshold = (active_record_bytes.saturating_mul(2))
            .clamp(MIN_COMPACT_STALE_BYTES, MAX_COMPACT_STALE_BYTES);
        Ok(stale_bytes > threshold)
    }

    fn active_record_bytes(&self) -> std::io::Result<u64> {
        let chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        self.chunks
            .iter()
            .filter(|(chunk_id, _)| **chunk_id < chunk_count)
            .try_fold(0_u64, |total, (_, entry)| {
                let allocated = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
                total
                    .checked_add(entry_size)
                    .and_then(|value| value.checked_add(allocated))
                    .ok_or_else(|| invalid_data("active container byte count overflow"))
            })
    }

    fn compaction_path(&self) -> PathBuf {
        self.path.with_extension(format!(
            "{}.compact.tmp",
            self.path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("uqac")
        ))
    }

    fn write_compacted_file(
        &mut self,
        path: &Path,
        generation: u64,
    ) -> std::io::Result<CompactedState> {
        let mut file = File::create(path)?;
        self.write_compacted_header(&mut file, generation)?;
        let (commit_offset, chunks, records) =
            self.write_compacted_chunks(&mut file, generation)?;
        let (append_offset, state_tag) =
            self.write_compacted_commit(&mut file, generation, commit_offset, &records)?;
        file.set_len(append_offset)?;
        file.sync_all()?;
        Ok(CompactedState {
            append_offset,
            chunks,
            state_tag,
        })
    }

    fn write_compacted_header(&self, file: &mut File, generation: u64) -> std::io::Result<()> {
        let flags = if self.keys.is_some() {
            FLAG_ENCRYPTED
        } else {
            0
        };
        file.write_all(&build_header(
            &self.header_metadata(flags, self.chunk_count()?, self.logical_len, generation),
            self.keys.as_ref().map(|keys| &keys.mac_key),
        )?)
    }

    fn write_compacted_chunks(
        &mut self,
        file: &mut File,
        generation: u64,
    ) -> std::io::Result<(
        u64,
        BTreeMap<u64, ChunkEntry>,
        Vec<AuthenticatedChunkRecord>,
    )> {
        let chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let chunk_ids = self
            .chunks
            .keys()
            .copied()
            .filter(|chunk_id| *chunk_id < chunk_count)
            .collect::<Vec<_>>();
        let mut offset = usize_to_u64(HEADER_SIZE, "container header size")?;
        let mut chunks = BTreeMap::new();
        let mut records = Vec::with_capacity(chunk_ids.len());
        for chunk_id in chunk_ids {
            let (entry, payload) = self.encode_dirty_chunk(chunk_id, offset, generation)?;
            file.write_all(&build_entry(&entry)?)?;
            file.write_all(&payload)?;
            offset = entry
                .offset
                .checked_add(usize_to_u64(
                    entry.allocated_len,
                    "compacted chunk allocated length",
                )?)
                .ok_or_else(|| invalid_data("compacted append offset overflow"))?;
            let payload_tag = if self.keys.is_some() {
                chunk_payload_tag(&payload)?
            } else {
                [0_u8; AEAD_TAG_LEN]
            };
            chunks.insert(chunk_id, entry.clone());
            records.push(AuthenticatedChunkRecord { entry, payload_tag });
        }
        Ok((offset, chunks, records))
    }

    fn write_compacted_commit(
        &self,
        file: &mut File,
        generation: u64,
        commit_offset: u64,
        records: &[AuthenticatedChunkRecord],
    ) -> std::io::Result<(u64, [u8; AUTH_TAG_LEN])> {
        let commit = build_commit_entry(
            generation,
            self.logical_len,
            self.chunk_count()?,
            self.keys.is_some(),
        )?;
        file.write_all(&build_entry(&commit)?)?;
        let state_tag = if let Some(keys) = &self.keys {
            let tag = commit_authentication_tag(
                &keys.mac_key,
                &self.file_id,
                commit_offset,
                &commit,
                &[0_u8; AUTH_TAG_LEN],
                records,
            )?;
            file.write_all(&tag)?;
            tag
        } else {
            [0_u8; AUTH_TAG_LEN]
        };
        let payload_len = usize_to_u64(
            commit.allocated_len,
            "compacted commit authentication tag length",
        )?;
        let append_offset = commit_offset
            .checked_add(usize_to_u64(ENTRY_SIZE, "container entry size")?)
            .and_then(|offset| offset.checked_add(payload_len))
            .ok_or_else(|| invalid_data("compacted commit offset overflow"))?;
        Ok((append_offset, state_tag))
    }
}
