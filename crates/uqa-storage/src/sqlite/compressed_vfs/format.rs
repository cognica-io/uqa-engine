//! Versioned container header and chunk-record encoding/validation.

use super::{
    chunk_count_for, ChunkEntry, Header, SQLiteCompressionCodec, SQLiteCompressionOptions,
    AEAD_TAG_LEN, CHUNK_COMPRESSED, CHUNK_ENCRYPTED, ENTRY_SIZE, FLAG_ENCRYPTED, HEADER_SIZE,
    MAGIC, NONCE_LEN, SALT_LEN, VERSION,
};

pub(super) fn allocate_payload(length: usize, context: &str) -> std::io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.try_reserve_exact(length).map_err(|error| {
        invalid_data(format!(
            "unable to allocate {context} of {length} bytes: {error}"
        ))
    })?;
    payload.resize(length, 0);
    Ok(payload)
}

pub(super) fn validate_chunk_entry(
    entry: &ChunkEntry,
    chunk_size: usize,
    container_encrypted: bool,
) -> std::io::Result<()> {
    if entry.flags & !(CHUNK_COMPRESSED | CHUNK_ENCRYPTED) != 0 {
        return Err(invalid_data("chunk entry contains unsupported flags"));
    }
    let entry_encrypted = entry.flags & CHUNK_ENCRYPTED != 0;
    if entry_encrypted != container_encrypted {
        return Err(invalid_data(
            "chunk encryption flag does not match the container header",
        ));
    }
    if entry.raw_len == 0 || entry.raw_len > chunk_size {
        return Err(invalid_data(
            "chunk raw length is outside the configured chunk size",
        ));
    }
    let overhead = if entry_encrypted { AEAD_TAG_LEN } else { 0 };
    let uncompressed_stored_len = entry
        .raw_len
        .checked_add(overhead)
        .ok_or_else(|| invalid_data("chunk stored length limit overflow"))?;
    if entry.flags & CHUNK_COMPRESSED != 0 {
        if entry.stored_len >= uncompressed_stored_len {
            return Err(invalid_data(
                "compressed chunk payload is not smaller than its raw payload",
            ));
        }
        if entry_encrypted && entry.stored_len < AEAD_TAG_LEN {
            return Err(invalid_data(
                "encrypted chunk payload is shorter than its tag",
            ));
        }
    } else if entry.stored_len != uncompressed_stored_len {
        return Err(invalid_data(
            "uncompressed chunk stored length does not match its raw payload",
        ));
    }
    if entry.allocated_len != entry.stored_len {
        return Err(invalid_data(
            "chunk allocation length does not match its stored payload",
        ));
    }
    Ok(())
}

pub(super) fn parse_header(bytes: &[u8]) -> std::io::Result<Header> {
    if bytes.len() < HEADER_SIZE {
        return Err(invalid_data("compressed container header is truncated"));
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(invalid_data("not a UQA compressed SQLite container"));
    }
    let version = read_u32(bytes, 8)?;
    if version != VERSION {
        return Err(invalid_data("unsupported compressed container version"));
    }
    let flags = read_u32(bytes, 12)?;
    if flags & !FLAG_ENCRYPTED != 0 {
        return Err(invalid_data("unsupported compressed container flags"));
    }
    let page_size = read_u32(bytes, 16)?;
    let chunk_pages = read_u32(bytes, 20)?;
    let level = read_i32(bytes, 24)?;
    let header_size = read_u32(bytes, 28)?;
    let entry_size = read_u32(bytes, 32)?;
    if usize::try_from(header_size).ok() != Some(HEADER_SIZE)
        || usize::try_from(entry_size).ok() != Some(ENTRY_SIZE)
    {
        return Err(invalid_data("unsupported compressed container layout"));
    }
    let chunk_count =
        usize::try_from(read_u64(bytes, 36)?).map_err(|_| invalid_data("chunk count"))?;
    let logical_len =
        usize::try_from(read_u64(bytes, 44)?).map_err(|_| invalid_data("logical length"))?;
    let generation = read_u64(bytes, 52)?;
    let mut salt = [0_u8; SALT_LEN];
    salt.copy_from_slice(&bytes[60..60 + SALT_LEN]);
    let codec = SQLiteCompressionCodec::from_id(read_u32(bytes, 76)?).map_err(invalid_data)?;
    let compression = SQLiteCompressionOptions {
        codec,
        page_size,
        chunk_pages,
        level,
    }
    .validate()
    .map_err(invalid_data)?;
    let expected_chunk_count =
        chunk_count_for(logical_len, compression.chunk_size().map_err(invalid_data)?);
    if chunk_count != expected_chunk_count {
        return Err(invalid_data("compressed container chunk count mismatch"));
    }
    Ok(Header {
        flags,
        compression,
        chunk_count,
        logical_len,
        generation,
        salt,
    })
}

pub(super) fn build_header(
    flags: u32,
    compression: SQLiteCompressionOptions,
    chunk_count: usize,
    logical_len: usize,
    generation: u64,
    salt: [u8; SALT_LEN],
) -> std::io::Result<[u8; HEADER_SIZE]> {
    let mut out = [0_u8; HEADER_SIZE];
    out[..MAGIC.len()].copy_from_slice(MAGIC);
    write_u32(&mut out, 8, VERSION);
    write_u32(&mut out, 12, flags);
    write_u32(&mut out, 16, compression.page_size);
    write_u32(&mut out, 20, compression.chunk_pages);
    write_i32(&mut out, 24, compression.level);
    write_u32(
        &mut out,
        28,
        u32::try_from(HEADER_SIZE).map_err(|_| invalid_data("container header size"))?,
    );
    write_u32(
        &mut out,
        32,
        u32::try_from(ENTRY_SIZE).map_err(|_| invalid_data("container entry size"))?,
    );
    write_u64(
        &mut out,
        36,
        usize_to_u64(chunk_count, "header chunk count")?,
    );
    write_u64(
        &mut out,
        44,
        usize_to_u64(logical_len, "header logical length")?,
    );
    write_u64(&mut out, 52, generation);
    out[60..60 + SALT_LEN].copy_from_slice(&salt);
    write_u32(&mut out, 76, compression.codec.id());
    Ok(out)
}

pub(super) fn parse_entry(bytes: &[u8]) -> std::io::Result<ChunkEntry> {
    let chunk_id = read_u64(bytes, 0)?;
    let offset = read_u64(bytes, 8)?;
    let stored_len =
        usize::try_from(read_u64(bytes, 16)?).map_err(|_| invalid_data("chunk stored length"))?;
    let raw_len =
        usize::try_from(read_u64(bytes, 24)?).map_err(|_| invalid_data("chunk raw length"))?;
    let flags = read_u32(bytes, 32)?;
    let crc32 = read_u32(bytes, 36)?;
    let mut nonce = [0_u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[40..40 + NONCE_LEN]);
    let generation = read_u64(bytes, 64)?;
    let allocated_len = usize::try_from(read_u64(bytes, 72)?)
        .map_err(|_| invalid_data("chunk allocated length"))?;
    Ok(ChunkEntry {
        chunk_id,
        offset,
        stored_len,
        raw_len,
        flags,
        crc32,
        nonce,
        generation,
        allocated_len,
    })
}

pub(super) fn build_entry(entry: &ChunkEntry) -> std::io::Result<[u8; ENTRY_SIZE]> {
    let mut out = [0_u8; ENTRY_SIZE];
    write_u64(&mut out, 0, entry.chunk_id);
    write_u64(&mut out, 8, entry.offset);
    write_u64(
        &mut out,
        16,
        usize_to_u64(entry.stored_len, "entry stored length")?,
    );
    write_u64(
        &mut out,
        24,
        usize_to_u64(entry.raw_len, "entry raw length")?,
    );
    write_u32(&mut out, 32, entry.flags);
    write_u32(&mut out, 36, entry.crc32);
    out[40..40 + NONCE_LEN].copy_from_slice(&entry.nonce);
    write_u64(&mut out, 64, entry.generation);
    write_u64(
        &mut out,
        72,
        usize_to_u64(entry.allocated_len, "entry allocated length")?,
    );
    Ok(out)
}

fn read_u32(bytes: &[u8], offset: usize) -> std::io::Result<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_data("u32 field truncated"))?;
    let encoded: [u8; 4] = slice
        .try_into()
        .map_err(|_| invalid_data("u32 field has an invalid width"))?;
    Ok(u32::from_le_bytes(encoded))
}

fn read_i32(bytes: &[u8], offset: usize) -> std::io::Result<i32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_data("i32 field truncated"))?;
    let encoded: [u8; 4] = slice
        .try_into()
        .map_err(|_| invalid_data("i32 field has an invalid width"))?;
    Ok(i32::from_le_bytes(encoded))
}

fn read_u64(bytes: &[u8], offset: usize) -> std::io::Result<u64> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid_data("u64 field truncated"))?;
    let encoded: [u8; 8] = slice
        .try_into()
        .map_err(|_| invalid_data("u64 field has an invalid width"))?;
    Ok(u64::from_le_bytes(encoded))
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

pub(super) fn usize_to_u64(value: usize, context: &str) -> std::io::Result<u64> {
    u64::try_from(value).map_err(|_| invalid_data(format!("{context} exceeds u64")))
}

pub(super) fn fill_random(dest: &mut [u8]) -> std::io::Result<()> {
    getrandom::fill(dest)
        .map_err(|err| std::io::Error::other(format!("failed to obtain OS randomness: {err}")))
}
