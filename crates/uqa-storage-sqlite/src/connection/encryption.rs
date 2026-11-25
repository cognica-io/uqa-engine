//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLCipher` physical connections of one file share its password-derived key.

#[cfg(not(target_os = "emscripten"))]
mod auxiliary;
#[cfg(all(test, not(target_os = "emscripten")))]
mod tests;
#[cfg(not(target_os = "emscripten"))]
pub(super) use auxiliary::DatabaseKeys;

#[cfg(not(target_os = "emscripten"))]
use std::fmt::Write;
use std::sync::Arc;

#[cfg(not(target_os = "emscripten"))]
use cipher::zeroize::Zeroizing;
use parking_lot::Mutex;
use rusqlite::Connection;
use uqa_storage::StorageEncryptionKey;

use super::{Result, SQLiteError};

#[derive(Clone)]
pub(super) struct FileKey {
    pub(super) supplied: StorageEncryptionKey,
    prepared: Arc<Mutex<Option<StorageEncryptionKey>>>,
}

impl From<StorageEncryptionKey> for FileKey {
    fn from(supplied: StorageEncryptionKey) -> Self {
        Self {
            supplied,
            prepared: Arc::default(),
        }
    }
}

impl FileKey {
    #[cfg(not(target_os = "emscripten"))]
    pub(super) fn auxiliary_key(
        &self,
        connection: &Connection,
        path: &std::path::Path,
        owner: Option<&Arc<super::ownership::DatabaseOwner>>,
    ) -> Result<StorageEncryptionKey> {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let prepared = self.prepared.lock();
        let hex = prepared
            .as_ref()
            .and_then(|key| raw_hex(key.expose_secret()))
            .ok_or_else(|| {
                SQLiteError::StorageBackend("SQLCipher file key is unavailable".into())
            })?;
        let key = Zeroizing::new(decode_hex::<32>(&hex[..64])?);
        let (_, salt) = file_salt(connection)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_slice()).expect("256-bit SQLite key");
        mac.update(b"UQA SQLCipher auxiliary keys v1");
        mac.update(&salt);
        let root = Zeroizing::new(mac.finalize().into_bytes().into());
        Ok(DatabaseKeys::credential(root, path, &self.supplied, owner))
    }

    pub(super) fn apply(&self, connection: &Connection) -> Result<()> {
        let mut prepared = self.prepared.lock();
        if let Some(key) = prepared.as_ref() {
            connection.pragma_update(None, "key", key.expose_secret())?;
            return Ok(());
        }
        if self.supplied.expose_secret().is_empty() {
            return Err(SQLiteError::EmptyEncryptionKey);
        }
        *prepared = Some(prepare(connection, &self.supplied)?);
        Ok(())
    }
}

#[cfg(not(target_os = "emscripten"))]
fn raw_hex(value: &str) -> Option<&str> {
    let text = value
        .strip_prefix("x'")
        .or_else(|| value.strip_prefix("X'"))?
        .strip_suffix('\'')?;
    (matches!(text.len(), 64 | 96 | 160) && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(text)
}

#[cfg(not(target_os = "emscripten"))]
fn raw_key(key: &[u8], salt: &str) -> StorageEncryptionKey {
    let mut value = Zeroizing::new(String::with_capacity(3 + 2 * key.len() + salt.len()));
    value.push_str("x'");
    for byte in key {
        write!(value, "{byte:02x}").expect("write to String");
    }
    value.push_str(salt);
    value.push('\'');
    StorageEncryptionKey::new(value.as_str())
}

#[cfg(not(target_os = "emscripten"))]
fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != 2 * N || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SQLiteError::StorageBackend(
            "invalid SQLCipher key or salt encoding".into(),
        ));
    }
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).expect("validated hexadecimal byte");
    }
    Ok(bytes)
}

#[cfg(not(target_os = "emscripten"))]
fn file_salt(connection: &Connection) -> Result<(String, [u8; 16])> {
    let hex: String = connection.pragma_query_value(None, "cipher_salt", |row| row.get(0))?;
    let salt = decode_hex(&hex)?;
    Ok((hex, salt))
}

#[cfg(all(test, not(target_os = "emscripten")))]
std::thread_local! {
    static PASSWORD_DERIVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(not(target_os = "emscripten"))]
fn prepare(
    connection: &Connection,
    supplied: &StorageEncryptionKey,
) -> Result<StorageEncryptionKey> {
    use ring::pbkdf2;
    use std::num::NonZeroU32;
    let password = supplied.expose_secret();
    connection.pragma_update(None, "key", password)?;
    if raw_hex(password).is_some() {
        return Ok(supplied.clone());
    }

    // These public configuration PRAGMAs read the codec settings before its first page read.
    // For a new file SQLCipher supplies a random salt; that exact salt must reach its header.
    let (hex_salt, salt) = file_salt(connection)?;
    let iterations: String = connection.pragma_query_value(None, "kdf_iter", |row| row.get(0))?;
    let iterations: NonZeroU32 = iterations
        .parse()
        .map_err(|_| SQLiteError::StorageBackend("invalid SQLCipher KDF iteration count".into()))?;
    let algorithm: String =
        connection.pragma_query_value(None, "cipher_kdf_algorithm", |row| row.get(0))?;
    let algorithm = match algorithm.as_str() {
        "PBKDF2_HMAC_SHA512" => pbkdf2::PBKDF2_HMAC_SHA512,
        "PBKDF2_HMAC_SHA256" => pbkdf2::PBKDF2_HMAC_SHA256,
        "PBKDF2_HMAC_SHA1" => pbkdf2::PBKDF2_HMAC_SHA1,
        _ => {
            return Err(SQLiteError::StorageBackend(
                "unsupported SQLCipher KDF algorithm".into(),
            ))
        }
    };
    let mut key = Zeroizing::new([0u8; 32]);
    #[cfg(test)]
    PASSWORD_DERIVATIONS.set(PASSWORD_DERIVATIONS.get() + 1);
    pbkdf2::derive(
        algorithm,
        iterations,
        &salt,
        password.as_bytes(),
        key.as_mut_slice(),
    );
    let raw = raw_key(key.as_slice(), &hex_salt);
    connection.pragma_update(None, "key", raw.expose_secret())?;
    Ok(raw)
}

#[cfg(target_os = "emscripten")]
fn prepare(
    _connection: &Connection,
    _supplied: &StorageEncryptionKey,
) -> Result<StorageEncryptionKey> {
    Err(SQLiteError::StorageBackend(
        "encrypted SQLite storage requires SQLCipher".into(),
    ))
}
