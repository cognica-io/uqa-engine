//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Language identities shared by routine metadata and catalog dependencies.

pub const INTERNAL_LANGUAGE: u32 = 12;
pub const C_LANGUAGE: u32 = 13;
pub const SQL_LANGUAGE: u32 = 14;
pub const PLPGSQL_LANGUAGE: u32 = 13_647;

pub const fn language_name(oid: u32) -> Option<&'static str> {
    match oid {
        INTERNAL_LANGUAGE => Some("internal"),
        C_LANGUAGE => Some("c"),
        SQL_LANGUAGE => Some("sql"),
        PLPGSQL_LANGUAGE => Some("plpgsql"),
        _ => None,
    }
}

pub fn language_oid(name: &str) -> Option<u32> {
    [
        INTERNAL_LANGUAGE,
        C_LANGUAGE,
        SQL_LANGUAGE,
        PLPGSQL_LANGUAGE,
    ]
    .into_iter()
    .find(|&oid| language_name(oid).is_some_and(|known| known.eq_ignore_ascii_case(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_language_names_preserve_postgresql_identities() {
        for (oid, name) in [
            (12, "internal"),
            (13, "c"),
            (14, "sql"),
            (13_647, "plpgsql"),
        ] {
            assert_eq!(language_name(oid), Some(name));
            assert_eq!(language_oid(name), Some(oid));
            assert_eq!(language_oid(&name.to_ascii_uppercase()), Some(oid));
        }
        assert_eq!(language_name(0), None);
        assert_eq!(language_oid("missing"), None);
    }
}
