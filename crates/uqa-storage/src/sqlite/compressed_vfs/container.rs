//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Append-only compressed-container lifecycle, chunk cache, and compaction.

use super::{
    allocate_payload, build_entry, build_header, chunk_aad, chunk_payload_tag,
    commit_authentication_tag, compress_chunk, decompress_chunk, fill_random, fs, invalid_data,
    keys_from_key, parse_header, scan_committed_records, usize_to_u64,
    verify_header_authentication, Aead, AuthenticatedChunkRecord, BTreeMap, BTreeSet, ChunkEntry,
    ContainerFile, File, HeaderMetadata, OpenOptions, OpenOptionsEntry, PathBuf, Payload, Read,
    SQLiteCompressedContainerAnchor, Seek, SeekFrom, Write, XNonce, AEAD_TAG_LEN, AUTH_TAG_LEN,
    CHUNK_AUTHENTICATED, CHUNK_COMMIT, CHUNK_COMPRESSED, CHUNK_ENCRYPTED, COMMIT_CHUNK_ID,
    ENTRY_SIZE, FILE_ID_LEN, FLAG_ENCRYPTED, HEADER_SIZE, NONCE_LEN, SALT_LEN,
};

impl ContainerFile {
    pub(super) fn open(path: PathBuf, options: OpenOptionsEntry) -> std::io::Result<Self> {
        if path.exists() && path.metadata()?.len() > 0 {
            return Self::load(path, options.key.as_deref());
        }
        let OpenOptionsEntry {
            compression, key, ..
        } = options;
        let mut salt = [0_u8; SALT_LEN];
        if key.is_some() {
            fill_random(&mut salt)?;
        }
        let mut file_id = [0_u8; FILE_ID_LEN];
        fill_random(&mut file_id)?;
        let keys = key
            .as_deref()
            .map(|key| keys_from_key(key, &salt))
            .transpose()?;
        Ok(Self {
            path,
            logical_len: 0,
            append_offset: usize_to_u64(HEADER_SIZE, "container header size")?,
            chunks: BTreeMap::new(),
            cache: BTreeMap::new(),
            dirty_chunks: BTreeSet::new(),
            compression,
            keys,
            salt,
            file_id,
            generation: 0,
            state_tag: [0_u8; AUTH_TAG_LEN],
            dirty_header: false,
        })
    }

    pub(super) fn load(path: PathBuf, key: Option<&str>) -> std::io::Result<Self> {
        let mut file = File::open(&path)?;
        let mut header_bytes = [0_u8; HEADER_SIZE];
        file.read_exact(&mut header_bytes)?;
        let header = parse_header(&header_bytes)?;
        let chunk_size = header.compression.chunk_size().map_err(invalid_data)?;
        let keys = key
            .map(|key| keys_from_key(key, &header.salt))
            .transpose()?;
        verify_header_authentication(
            &header_bytes,
            &header,
            keys.as_ref().map(|keys| &keys.mac_key),
        )?;
        let committed = scan_committed_records(&mut file, &header, keys.as_ref(), chunk_size)?;
        Ok(Self {
            path,
            logical_len: committed.logical_len,
            append_offset: committed.end_offset,
            chunks: committed.chunks,
            cache: BTreeMap::new(),
            dirty_chunks: BTreeSet::new(),
            compression: header.compression,
            keys,
            salt: header.salt,
            file_id: header.file_id,
            generation: committed.generation,
            state_tag: committed.state_tag,
            dirty_header: false,
        })
    }

    pub(super) fn authenticated_anchor(&self) -> SQLiteCompressedContainerAnchor {
        SQLiteCompressedContainerAnchor {
            database_id: self.file_id,
            generation: self.generation,
            state_tag: self.state_tag,
        }
    }

    pub(super) fn require_trusted_anchor(
        &self,
        trusted: SQLiteCompressedContainerAnchor,
    ) -> std::io::Result<()> {
        let current = self.authenticated_anchor();
        if current.database_id != trusted.database_id {
            return Err(invalid_data(
                "compressed container identity does not match the trusted anchor",
            ));
        }
        if current.generation != trusted.generation {
            return Err(invalid_data(format!(
                "compressed container generation {} does not match trusted generation {}",
                current.generation, trusted.generation
            )));
        }
        if current.state_tag != trusted.state_tag {
            return Err(invalid_data(
                "compressed container state does not match the trusted anchor",
            ));
        }
        Ok(())
    }

    pub(super) fn flush(&mut self) -> std::io::Result<()> {
        if self.dirty_chunks.is_empty() && !self.dirty_header {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let next_generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("container generation overflow"))?;
        let encrypted = self.keys.is_some();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        self.ensure_header(&mut file, encrypted)?;
        file.set_len(self.append_offset)?;
        file.seek(SeekFrom::Start(self.append_offset))?;
        let active_chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let dirty_chunks: Vec<u64> = self
            .dirty_chunks
            .iter()
            .copied()
            .filter(|chunk_id| *chunk_id < active_chunk_count)
            .collect();
        let mut append_offset = self.append_offset;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut pending_records = Vec::with_capacity(dirty_chunks.len());
        for chunk_id in dirty_chunks {
            let (entry, stored) =
                self.encode_dirty_chunk(chunk_id, append_offset, next_generation)?;
            file.write_all(&build_entry(&entry)?)?;
            file.write_all(&stored)?;
            let allocated_len = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
            append_offset = append_offset
                .checked_add(entry_size)
                .and_then(|offset| offset.checked_add(allocated_len))
                .ok_or_else(|| invalid_data("container append offset overflow"))?;
            let payload_tag = if encrypted {
                chunk_payload_tag(&stored)?
            } else {
                [0_u8; AEAD_TAG_LEN]
            };
            pending_records.push(AuthenticatedChunkRecord { entry, payload_tag });
        }
        let commit_offset = append_offset;
        let commit = build_commit_entry(
            next_generation,
            self.logical_len,
            self.chunk_count()?,
            encrypted,
        )?;
        file.write_all(&build_entry(&commit)?)?;
        let next_state_tag = if let Some(keys) = &self.keys {
            let tag = commit_authentication_tag(
                &keys.mac_key,
                &self.file_id,
                commit_offset,
                &commit,
                &self.state_tag,
                &pending_records,
            )?;
            file.write_all(&tag)?;
            tag
        } else {
            [0_u8; AUTH_TAG_LEN]
        };
        let commit_payload_len = usize_to_u64(
            commit.allocated_len,
            "container commit authentication tag length",
        )?;
        append_offset = append_offset
            .checked_add(entry_size)
            .and_then(|offset| offset.checked_add(commit_payload_len))
            .ok_or_else(|| invalid_data("container commit offset overflow"))?;
        file.flush()?;
        file.sync_all()?;
        self.generation = next_generation;
        self.state_tag = next_state_tag;
        self.append_offset = append_offset;
        for record in pending_records {
            self.chunks.insert(record.entry.chunk_id, record.entry);
        }
        self.chunks
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.dirty_chunks.clear();
        self.dirty_header = false;
        self.write_current_header(&mut file)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        self.compact_if_needed()?;
        Ok(())
    }

    pub(super) fn ensure_header(&self, file: &mut File, encrypted: bool) -> std::io::Result<()> {
        let header_size = usize_to_u64(HEADER_SIZE, "container header size")?;
        if file.metadata()?.len() >= header_size {
            return Ok(());
        }
        file.set_len(header_size)?;
        file.seek(SeekFrom::Start(0))?;
        let header_chunk_count = if self.generation == 0 {
            0
        } else {
            self.chunk_count()?
        };
        let header_logical_len = if self.generation == 0 {
            0
        } else {
            self.logical_len
        };
        file.write_all(&build_header(
            &self.header_metadata(
                if encrypted { FLAG_ENCRYPTED } else { 0 },
                header_chunk_count,
                header_logical_len,
                self.generation,
            ),
            self.keys.as_ref().map(|keys| &keys.mac_key),
        )?)
    }

    fn write_current_header(&self, file: &mut File) -> std::io::Result<()> {
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&build_header(
            &self.header_metadata(
                if self.keys.is_some() {
                    FLAG_ENCRYPTED
                } else {
                    0
                },
                self.chunk_count()?,
                self.logical_len,
                self.generation,
            ),
            self.keys.as_ref().map(|keys| &keys.mac_key),
        )?)
    }

    pub(super) fn header_metadata(
        &self,
        flags: u32,
        chunk_count: usize,
        logical_len: usize,
        generation: u64,
    ) -> HeaderMetadata {
        HeaderMetadata {
            flags,
            compression: self.compression,
            chunk_count,
            logical_len,
            generation,
            salt: self.salt,
            file_id: self.file_id,
        }
    }

    pub(super) fn encode_dirty_chunk(
        &mut self,
        chunk_id: u64,
        append_offset: u64,
        generation: u64,
    ) -> std::io::Result<(ChunkEntry, Vec<u8>)> {
        let raw = self.load_chunk(chunk_id)?.clone();
        let compressed = compress_chunk(self.compression, &raw)?;
        let mut flags = 0_u32;
        let mut stored = if compressed.len() < raw.len() {
            flags |= CHUNK_COMPRESSED;
            compressed
        } else {
            raw.clone()
        };
        let mut nonce = [0_u8; NONCE_LEN];
        if self.keys.is_some() {
            flags |= CHUNK_ENCRYPTED;
            fill_random(&mut nonce)?;
        }
        let stored_len = stored
            .len()
            .checked_add(if self.keys.is_some() {
                super::AEAD_TAG_LEN
            } else {
                0
            })
            .ok_or_else(|| invalid_data("encrypted chunk length overflow"))?;
        let entry = ChunkEntry {
            chunk_id,
            offset: append_offset
                .checked_add(usize_to_u64(ENTRY_SIZE, "container entry size")?)
                .ok_or_else(|| invalid_data("chunk payload offset overflow"))?,
            stored_len,
            raw_len: raw.len(),
            flags,
            crc32: crc32fast::hash(&raw),
            nonce,
            generation,
            allocated_len: stored_len,
        };
        if let Some(keys) = &self.keys {
            let aad = chunk_aad(&self.file_id, append_offset, &entry)?;
            stored = keys
                .cipher
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &stored,
                        aad: &aad,
                    },
                )
                .map_err(|_| invalid_data("chunk encryption failed"))?;
        }
        if stored.len() != stored_len {
            return Err(invalid_data("encrypted chunk length mismatch"));
        }
        Ok((entry, stored))
    }

    pub(super) fn read_at(&mut self, offset: usize, dest: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.logical_len {
            dest.fill(0);
            return Ok(0);
        }
        let available = (self.logical_len - offset).min(dest.len());
        let chunk_size = self.compression.chunk_size().map_err(invalid_data)?;
        let mut copied = 0;
        while copied < available {
            let logical_offset = offset + copied;
            let chunk_id = usize_to_u64(logical_offset / chunk_size, "read chunk id")?;
            let chunk_offset = logical_offset % chunk_size;
            let copy_len = (available - copied).min(chunk_size - chunk_offset);
            let chunk = self.load_chunk(chunk_id)?;
            dest[copied..copied + copy_len]
                .copy_from_slice(&chunk[chunk_offset..chunk_offset + copy_len]);
            copied += copy_len;
        }
        dest[available..].fill(0);
        Ok(available)
    }

    pub(super) fn write_at(&mut self, offset: usize, source: &[u8]) -> std::io::Result<()> {
        if source.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(source.len())
            .ok_or_else(|| invalid_data("write offset overflow"))?;
        if end > self.logical_len {
            self.logical_len = end;
            self.dirty_header = true;
        }
        let chunk_size = self.compression.chunk_size().map_err(invalid_data)?;
        let mut copied = 0;
        while copied < source.len() {
            let logical_offset = offset + copied;
            let chunk_id = usize_to_u64(logical_offset / chunk_size, "write chunk id")?;
            let chunk_offset = logical_offset % chunk_size;
            let copy_len = (source.len() - copied).min(chunk_size - chunk_offset);
            let chunk = self.load_chunk(chunk_id)?;
            chunk[chunk_offset..chunk_offset + copy_len]
                .copy_from_slice(&source[copied..copied + copy_len]);
            self.dirty_chunks.insert(chunk_id);
            copied += copy_len;
        }
        Ok(())
    }

    pub(super) fn truncate_to(&mut self, size: usize) -> std::io::Result<()> {
        if size == self.logical_len {
            return Ok(());
        }
        let old_len = self.logical_len;
        let old_chunk_count = self.chunk_count()?;
        self.logical_len = size;
        self.dirty_header = true;
        let new_chunk_count = self.chunk_count()?;
        let active_chunk_count = usize_to_u64(new_chunk_count, "active chunk count")?;
        self.chunks
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.cache
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.dirty_chunks
            .retain(|chunk_id| *chunk_id < active_chunk_count);
        if size > 0 {
            let chunk_size = self.compression.chunk_size().map_err(invalid_data)?;
            let changed_chunk = if size < old_len && !size.is_multiple_of(chunk_size) {
                Some(usize_to_u64(new_chunk_count - 1, "changed chunk id")?)
            } else if old_len > 0 && !old_len.is_multiple_of(chunk_size) {
                Some(usize_to_u64(old_chunk_count - 1, "changed chunk id")?)
            } else if old_chunk_count == new_chunk_count {
                Some(usize_to_u64(new_chunk_count - 1, "changed chunk id")?)
            } else {
                None
            };
            if let Some(chunk_id) = changed_chunk {
                let expected_len = self.expected_chunk_len(chunk_id)?;
                let chunk = self.load_chunk(chunk_id)?;
                chunk.resize(expected_len, 0);
                self.dirty_chunks.insert(chunk_id);
            }
        }
        Ok(())
    }

    pub(super) fn load_chunk(&mut self, chunk_id: u64) -> std::io::Result<&mut Vec<u8>> {
        if !self.cache.contains_key(&chunk_id) {
            let raw = self.read_chunk_from_disk(chunk_id)?;
            self.cache.insert(chunk_id, raw);
        }
        let expected_len = self.expected_chunk_len(chunk_id)?;
        let chunk = self.cache.get_mut(&chunk_id).ok_or_else(|| {
            invalid_data(format!(
                "chunk {chunk_id} disappeared from the cache after loading"
            ))
        })?;
        chunk.resize(expected_len, 0);
        Ok(chunk)
    }

    pub(super) fn read_chunk_from_disk(&self, chunk_id: u64) -> std::io::Result<Vec<u8>> {
        let expected_len = self.expected_chunk_len(chunk_id)?;
        if expected_len == 0 {
            return Ok(Vec::new());
        }
        let Some(entry) = self.chunks.get(&chunk_id) else {
            return Ok(vec![0_u8; expected_len]);
        };
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut payload = allocate_payload(entry.stored_len, "chunk stored payload")?;
        file.read_exact(&mut payload)?;
        if entry.flags & CHUNK_ENCRYPTED != 0 {
            let keys = self
                .keys
                .as_ref()
                .ok_or_else(|| invalid_data("encrypted chunk without key"))?;
            let record_offset = entry
                .offset
                .checked_sub(usize_to_u64(ENTRY_SIZE, "container entry size")?)
                .ok_or_else(|| invalid_data("chunk record offset underflow"))?;
            let aad = chunk_aad(&self.file_id, record_offset, entry)?;
            payload = keys
                .cipher
                .decrypt(
                    XNonce::from_slice(&entry.nonce),
                    Payload {
                        msg: &payload,
                        aad: &aad,
                    },
                )
                .map_err(|_| invalid_data("compressed container chunk authentication failed"))?;
        }
        let mut raw = if entry.flags & CHUNK_COMPRESSED != 0 {
            decompress_chunk(self.compression.codec, &payload, entry.raw_len)?
        } else {
            payload
        };
        if raw.len() != entry.raw_len {
            return Err(invalid_data("chunk raw length mismatch"));
        }
        if crc32fast::hash(&raw) != entry.crc32 {
            return Err(invalid_data("chunk checksum mismatch"));
        }
        if raw.len() > expected_len {
            raw.truncate(expected_len);
        } else {
            raw.resize(expected_len, 0);
        }
        Ok(raw)
    }

    pub(super) fn chunk_count(&self) -> std::io::Result<usize> {
        Ok(chunk_count_for(
            self.logical_len,
            self.compression.chunk_size().map_err(invalid_data)?,
        ))
    }

    pub(super) fn expected_chunk_len(&self, chunk_id: u64) -> std::io::Result<usize> {
        Ok(expected_chunk_len_for(
            self.logical_len,
            self.compression.chunk_size().map_err(invalid_data)?,
            usize::try_from(chunk_id).map_err(|_| invalid_data("chunk id exceeds usize"))?,
        ))
    }
}

pub(super) fn chunk_count_for(logical_len: usize, chunk_size: usize) -> usize {
    if logical_len == 0 {
        0
    } else {
        logical_len.div_ceil(chunk_size)
    }
}

fn expected_chunk_len_for(logical_len: usize, chunk_size: usize, chunk_id: usize) -> usize {
    let chunk_count = chunk_count_for(logical_len, chunk_size);
    if chunk_id >= chunk_count {
        return 0;
    }
    if chunk_id + 1 < chunk_count {
        return chunk_size;
    }
    logical_len - chunk_id * chunk_size
}

pub(super) fn build_commit_entry(
    generation: u64,
    logical_len: usize,
    chunk_count: usize,
    encrypted: bool,
) -> std::io::Result<ChunkEntry> {
    let authentication_len = if encrypted { AUTH_TAG_LEN } else { 0 };
    Ok(ChunkEntry {
        chunk_id: COMMIT_CHUNK_ID,
        offset: usize_to_u64(logical_len, "commit logical length")?,
        stored_len: authentication_len,
        raw_len: chunk_count,
        flags: CHUNK_COMMIT | if encrypted { CHUNK_AUTHENTICATED } else { 0 },
        crc32: 0,
        nonce: [0_u8; NONCE_LEN],
        generation,
        allocated_len: authentication_len,
    })
}
