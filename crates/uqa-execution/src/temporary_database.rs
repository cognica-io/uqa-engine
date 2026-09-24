//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private execution indexes use independently keyed temporary databases.

use std::path::Path;

/// Native execution links bundled `SQLCipher`. A fresh raw key protects each private index without borrowing a database credential or deriving a passphrase. Browser `SQLite` has no encrypted provider and retains its existing temporary-database behavior.
pub(crate) fn open(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open(path)?;
    #[cfg(not(target_os = "emscripten"))]
    configure_ephemeral_key(&connection)?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(connection)
}

#[cfg(not(target_os = "emscripten"))]
fn configure_ephemeral_key(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key).map_err(|error| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(error.to_string())))
    })?;
    let mut raw_key = [0_u8; 67];
    raw_key[..2].copy_from_slice(b"x'");
    for (index, byte) in key.iter().copied().enumerate() {
        raw_key[2 + index * 2] = HEX[usize::from(byte >> 4)];
        raw_key[3 + index * 2] = HEX[usize::from(byte & 15)];
    }
    raw_key[66] = b'\'';
    key.fill(0);
    let configured = connection.pragma_update(
        None,
        "key",
        std::str::from_utf8(&raw_key).expect("raw cipher keys contain only ASCII"),
    );
    raw_key.fill(0);
    configured?;
    let version: String =
        connection.pragma_query_value(None, "cipher_version", |row| row.get(0))?;
    if version.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
