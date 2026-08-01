//! Append-only compressed-container lifecycle, chunk cache, and compaction.

use super::{
    allocate_payload, build_entry, build_header, cipher_from_key, compress_chunk, decompress_chunk,
    fill_random, fs, invalid_data, parse_entry, parse_header, usize_to_u64, validate_chunk_entry,
    Aead, BTreeMap, BTreeSet, ChunkEntry, ContainerFile, File, OpenOptions, OpenOptionsEntry,
    PathBuf, Payload, Read, Seek, SeekFrom, Write, XChaCha20Poly1305, XNonce, CHUNK_COMMIT,
    CHUNK_COMPRESSED, CHUNK_ENCRYPTED, COMMIT_CHUNK_ID, ENTRY_SIZE, FLAG_ENCRYPTED, HEADER_SIZE,
    MAX_COMPACT_STALE_BYTES, MIN_COMPACT_STALE_BYTES, NONCE_LEN, SALT_LEN,
};

impl ContainerFile {
    pub(super) fn open(path: PathBuf, options: OpenOptionsEntry) -> std::io::Result<Self> {
        if path.exists() && path.metadata()?.len() > 0 {
            return Self::load(path, options.key);
        }
        let mut salt = [0_u8; SALT_LEN];
        if options.key.is_some() {
            fill_random(&mut salt)?;
        }
        Ok(Self {
            path,
            logical_len: 0,
            append_offset: usize_to_u64(HEADER_SIZE, "container header size")?,
            chunks: BTreeMap::new(),
            cache: BTreeMap::new(),
            dirty_chunks: BTreeSet::new(),
            compression: options.compression,
            key: options.key,
            salt,
            generation: 0,
            dirty_header: false,
        })
    }

    pub(super) fn load(path: PathBuf, key: Option<String>) -> std::io::Result<Self> {
        let mut file = File::open(&path)?;
        let mut header_bytes = [0_u8; HEADER_SIZE];
        file.read_exact(&mut header_bytes)?;
        let header = parse_header(&header_bytes)?;
        let chunk_size = header.compression.chunk_size().map_err(invalid_data)?;
        let encrypted = header.flags & FLAG_ENCRYPTED != 0;
        if encrypted && key.is_none() {
            return Err(invalid_data(
                "compressed container requires an encryption key",
            ));
        }
        let file_len = file.metadata()?.len();
        let mut committed_generation = header.generation;
        let mut committed_logical_len = header.logical_len;
        let mut committed_chunk_count = header.chunk_count;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut record_offset = usize_to_u64(HEADER_SIZE, "container header size")?;
        let mut entries = Vec::new();
        loop {
            let entry_end = record_offset
                .checked_add(entry_size)
                .ok_or_else(|| invalid_data("container entry offset overflow"))?;
            if entry_end > file_len {
                break;
            }
            file.seek(SeekFrom::Start(record_offset))?;
            let mut entry_bytes = [0_u8; ENTRY_SIZE];
            file.read_exact(&mut entry_bytes)?;
            let entry = parse_entry(&entry_bytes)?;
            if entry.flags & CHUNK_COMMIT != 0 {
                if entry.chunk_id != COMMIT_CHUNK_ID
                    || entry.flags != CHUNK_COMMIT
                    || entry.stored_len != 0
                    || entry.allocated_len != 0
                {
                    return Err(invalid_data("invalid compressed container commit record"));
                }
                if entry.generation > committed_generation {
                    committed_generation = entry.generation;
                    committed_logical_len = usize::try_from(entry.offset)
                        .map_err(|_| invalid_data("commit logical length"))?;
                    committed_chunk_count = entry.raw_len;
                }
                record_offset = entry_end;
                continue;
            }
            validate_chunk_entry(&entry, chunk_size, encrypted)?;
            let payload_offset = entry_end;
            let allocated_len = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
            let payload_end = payload_offset
                .checked_add(allocated_len)
                .ok_or_else(|| invalid_data("chunk payload offset overflow"))?;
            if payload_end > file_len {
                break;
            }
            if entry.offset != payload_offset {
                return Err(invalid_data("chunk payload offset mismatch"));
            }
            entries.push(entry);
            record_offset = payload_end;
        }
        let expected_chunk_count = chunk_count_for(committed_logical_len, chunk_size);
        if committed_chunk_count != expected_chunk_count {
            return Err(invalid_data(
                "compressed container commit chunk count mismatch",
            ));
        }
        let committed_chunk_count = usize_to_u64(committed_chunk_count, "committed chunk count")?;
        let mut chunks = BTreeMap::new();
        for entry in entries {
            if entry.generation <= committed_generation && entry.chunk_id < committed_chunk_count {
                chunks.insert(entry.chunk_id, entry);
            }
        }
        Ok(Self {
            path,
            logical_len: committed_logical_len,
            append_offset: record_offset,
            chunks,
            cache: BTreeMap::new(),
            dirty_chunks: BTreeSet::new(),
            compression: header.compression,
            key,
            salt: header.salt,
            generation: committed_generation,
            dirty_header: false,
        })
    }

    pub(super) fn flush(&mut self) -> std::io::Result<()> {
        if self.dirty_chunks.is_empty() && !self.dirty_header {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.key.is_some() && self.salt == [0_u8; SALT_LEN] {
            fill_random(&mut self.salt)?;
        }
        let next_generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("container generation overflow"))?;
        let cipher = self
            .key
            .as_deref()
            .map(|key| cipher_from_key(key, &self.salt))
            .transpose()?;
        let encrypted = cipher.is_some();
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
        let mut pending_entries = Vec::with_capacity(dirty_chunks.len());
        for chunk_id in dirty_chunks {
            let (entry, stored) =
                self.encode_dirty_chunk(chunk_id, append_offset, next_generation, cipher.as_ref())?;
            file.write_all(&build_entry(&entry)?)?;
            file.write_all(&stored)?;
            let allocated_len = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
            append_offset = append_offset
                .checked_add(entry_size)
                .and_then(|offset| offset.checked_add(allocated_len))
                .ok_or_else(|| invalid_data("container append offset overflow"))?;
            pending_entries.push(entry);
        }
        let commit = build_commit_entry(next_generation, self.logical_len, self.chunk_count()?)?;
        file.write_all(&build_entry(&commit)?)?;
        append_offset = append_offset
            .checked_add(entry_size)
            .ok_or_else(|| invalid_data("container commit offset overflow"))?;
        file.flush()?;
        file.sync_all()?;
        self.generation = next_generation;
        self.append_offset = append_offset;
        drop(file);
        for entry in pending_entries {
            self.chunks.insert(entry.chunk_id, entry);
        }
        self.chunks
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.dirty_chunks.clear();
        self.dirty_header = false;
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
            if encrypted { FLAG_ENCRYPTED } else { 0 },
            self.compression,
            header_chunk_count,
            header_logical_len,
            self.generation,
            self.salt,
        )?)
    }

    pub(super) fn encode_dirty_chunk(
        &mut self,
        chunk_id: u64,
        append_offset: u64,
        generation: u64,
        cipher: Option<&XChaCha20Poly1305>,
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
        if let Some(cipher) = cipher {
            flags |= CHUNK_ENCRYPTED;
            fill_random(&mut nonce)?;
            stored = cipher
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &stored,
                        aad: &chunk_id.to_le_bytes(),
                    },
                )
                .map_err(|_| invalid_data("chunk encryption failed"))?;
        }
        let stored_len = stored.len();
        Ok((
            ChunkEntry {
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
            },
            stored,
        ))
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
            let changed_chunk = if size < old_len && size % chunk_size != 0 {
                Some(usize_to_u64(new_chunk_count - 1, "changed chunk id")?)
            } else if old_len > 0 && old_len % chunk_size != 0 {
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
            let cipher = cipher_from_key(
                self.key
                    .as_deref()
                    .ok_or_else(|| invalid_data("encrypted chunk without key"))?,
                &self.salt,
            )?;
            payload = cipher
                .decrypt(
                    XNonce::from_slice(&entry.nonce),
                    Payload {
                        msg: &payload,
                        aad: &entry.chunk_id.to_le_bytes(),
                    },
                )
                .map_err(|_| invalid_data("invalid compressed container encryption key"))?;
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

    pub(super) fn active_record_bytes(&self) -> std::io::Result<u64> {
        let chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut total = 0_u64;
        for entry in self
            .chunks
            .iter()
            .filter(|(chunk_id, _)| **chunk_id < chunk_count)
            .map(|(_, entry)| entry)
        {
            let allocated_len = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
            total = total
                .checked_add(entry_size)
                .and_then(|value| value.checked_add(allocated_len))
                .ok_or_else(|| invalid_data("active container byte count overflow"))?;
        }
        Ok(total)
    }

    pub(super) fn compact_if_needed(&mut self) -> std::io::Result<()> {
        let active_record_bytes = self.active_record_bytes()?;
        let compact_len = usize_to_u64(HEADER_SIZE, "container header size")?
            .checked_add(active_record_bytes)
            .ok_or_else(|| invalid_data("compacted container length overflow"))?;
        let stale_bytes = self.append_offset.saturating_sub(compact_len);
        let compact_threshold = (active_record_bytes.saturating_mul(2))
            .clamp(MIN_COMPACT_STALE_BYTES, MAX_COMPACT_STALE_BYTES);
        if stale_bytes <= compact_threshold {
            return Ok(());
        }
        let tmp_path = self.path.with_extension(format!(
            "{}.compact.tmp",
            self.path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("uqac")
        ));
        let mut source = File::open(&self.path)?;
        let mut tmp = File::create(&tmp_path)?;
        tmp.write_all(&build_header(
            if self.key.is_some() {
                FLAG_ENCRYPTED
            } else {
                0
            },
            self.compression,
            self.chunk_count()?,
            self.logical_len,
            self.generation,
            self.salt,
        )?)?;
        let chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut append_offset = usize_to_u64(HEADER_SIZE, "container header size")?;
        let mut compacted = BTreeMap::new();
        for (&chunk_id, entry) in self.chunks.iter().filter(|(id, _)| **id < chunk_count) {
            let mut payload = allocate_payload(entry.stored_len, "compacted chunk payload")?;
            source.seek(SeekFrom::Start(entry.offset))?;
            source.read_exact(&mut payload)?;
            let mut compacted_entry = entry.clone();
            compacted_entry.offset = append_offset
                .checked_add(entry_size)
                .ok_or_else(|| invalid_data("compacted payload offset overflow"))?;
            compacted_entry.allocated_len = compacted_entry.stored_len;
            tmp.write_all(&build_entry(&compacted_entry)?)?;
            tmp.write_all(&payload)?;
            append_offset = compacted_entry
                .offset
                .checked_add(usize_to_u64(
                    compacted_entry.allocated_len,
                    "compacted chunk allocated length",
                )?)
                .ok_or_else(|| invalid_data("compacted append offset overflow"))?;
            compacted.insert(chunk_id, compacted_entry);
        }
        tmp.set_len(append_offset)?;
        tmp.sync_all()?;
        drop(tmp);
        fs::rename(&tmp_path, &self.path)?;
        self.append_offset = append_offset;
        self.chunks = compacted;
        Ok(())
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

fn build_commit_entry(
    generation: u64,
    logical_len: usize,
    chunk_count: usize,
) -> std::io::Result<ChunkEntry> {
    Ok(ChunkEntry {
        chunk_id: COMMIT_CHUNK_ID,
        offset: usize_to_u64(logical_len, "commit logical length")?,
        stored_len: 0,
        raw_len: chunk_count,
        flags: CHUNK_COMMIT,
        crc32: 0,
        nonce: [0_u8; NONCE_LEN],
        generation,
        allocated_len: 0,
    })
}
