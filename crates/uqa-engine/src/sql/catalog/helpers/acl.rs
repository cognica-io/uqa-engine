//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` ACL text rendering helpers.

pub(in crate::sql::catalog) fn acl_identifier(name: &str) -> String {
    if name.bytes().enumerate().all(|(index, byte)| {
        byte == b'_' || byte.is_ascii_lowercase() || index > 0 && byte.is_ascii_digit()
    }) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}
