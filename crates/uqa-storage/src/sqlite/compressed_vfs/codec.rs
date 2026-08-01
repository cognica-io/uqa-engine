//! Chunk compression, bounded decompression, and authenticated encryption.

use super::{
    allocate_payload, invalid_data, Argon2, Block, KeyInit, Read, SQLiteCompressionCodec,
    SQLiteCompressionOptions, XChaCha20Poly1305, SALT_LEN,
};

pub(super) fn cipher_from_key(
    key: &str,
    salt: &[u8; SALT_LEN],
) -> std::io::Result<XChaCha20Poly1305> {
    let mut derived = [0_u8; 32];
    let argon2 = Argon2::default();
    let mut memory_blocks = vec![Block::default(); argon2.params().block_count()];
    argon2
        .hash_password_into_with_memory(key.as_bytes(), salt, &mut derived, &mut memory_blocks)
        .map_err(|_| invalid_data("failed to derive compressed container key"))?;
    XChaCha20Poly1305::new_from_slice(&derived)
        .map_err(|_| invalid_data("failed to initialize compressed container cipher"))
}

pub(super) fn compress_chunk(
    compression: SQLiteCompressionOptions,
    raw: &[u8],
) -> std::io::Result<Vec<u8>> {
    match compression.codec {
        SQLiteCompressionCodec::Zstd => zstd::stream::encode_all(raw, compression.level),
        SQLiteCompressionCodec::LZ4 => Ok(lz4_flex::compress_prepend_size(raw)),
    }
}

pub(super) fn decompress_chunk(
    codec: SQLiteCompressionCodec,
    payload: &[u8],
    expected_len: usize,
) -> std::io::Result<Vec<u8>> {
    match codec {
        SQLiteCompressionCodec::Zstd => {
            let decoder = zstd::stream::read::Decoder::new(payload)?;
            let read_limit = u64::try_from(expected_len)
                .ok()
                .and_then(|length| length.checked_add(1))
                .ok_or_else(|| invalid_data("zstd decoded length limit overflow"))?;
            let mut output = Vec::new();
            output.try_reserve_exact(expected_len).map_err(|error| {
                invalid_data(format!(
                    "unable to allocate zstd output of {expected_len} bytes: {error}"
                ))
            })?;
            decoder.take(read_limit).read_to_end(&mut output)?;
            if output.len() != expected_len {
                return Err(invalid_data("zstd decoded length mismatch"));
            }
            Ok(output)
        }
        SQLiteCompressionCodec::LZ4 => {
            let encoded_len = payload
                .get(..4)
                .ok_or_else(|| invalid_data("lz4 payload is missing its decoded length"))?;
            let mut encoded = [0_u8; 4];
            encoded.copy_from_slice(encoded_len);
            let declared_len = usize::try_from(u32::from_le_bytes(encoded))
                .map_err(|_| invalid_data("lz4 decoded length is outside address space"))?;
            if declared_len != expected_len {
                return Err(invalid_data("lz4 decoded length mismatch"));
            }
            let mut output = allocate_payload(expected_len, "lz4 decoded chunk")?;
            let decoded = lz4_flex::decompress_into(&payload[4..], &mut output)
                .map_err(|error| invalid_data(error.to_string()))?;
            if decoded != expected_len {
                return Err(invalid_data("lz4 decoded byte count mismatch"));
            }
            Ok(output)
        }
    }
}
