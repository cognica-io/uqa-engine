//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Names of the array types generated for user-defined types, as `PostgreSQL`'s `makeArrayTypeName` chooses them.

/// `PostgreSQL` stores type names in a `name` column of `NAMEDATALEN - 1` bytes.
const MAX_TYPE_NAME_BYTES: usize = 63;

/// A persisted `PostgreSQL` type name is a nonempty, NUL-free `name` value, at most 63 UTF-8 bytes.
pub fn valid_type_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_TYPE_NAME_BYTES && !name.contains('\0')
}

/// `PostgreSQL`'s `makeArrayTypeName`: an underscore-prefixed name for `pass == 0`, then a numeric suffix, clipping the base name at a UTF-8 boundary so the result fits 63 bytes.
pub fn array_type_name(type_name: &str, pass: u32) -> String {
    let suffix = (pass > 0).then(|| pass.to_string());
    let overhead = 1 + suffix.as_ref().map_or(0, |suffix| suffix.len() + 1);
    let mut length = type_name.len().min(MAX_TYPE_NAME_BYTES - overhead);
    while !type_name.is_char_boundary(length) {
        length -= 1;
    }
    let mut name = String::with_capacity(length + overhead);
    name.push('_');
    name.push_str(&type_name[..length]);
    if let Some(suffix) = suffix {
        name.push('_');
        name.push_str(&suffix);
    }
    name
}

/// Select the first generated array name that no existing type in the namespace uses.
pub fn choose_array_type_name(type_name: &str, mut in_use: impl FnMut(&str) -> bool) -> String {
    let mut pass = 0;
    loop {
        let name = array_type_name(type_name, pass);
        if !in_use(&name) {
            return name;
        }
        pass += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{array_type_name, choose_array_type_name, valid_type_name};

    #[test]
    fn stored_type_names_use_the_name_types_byte_limit() {
        assert!(valid_type_name(&"x".repeat(63)));
        assert!(valid_type_name("_quoted name"));
        assert!(!valid_type_name(""));
        assert!(!valid_type_name("a\0b"));
        assert!(!valid_type_name(&"é".repeat(32)));
    }

    #[test]
    fn array_names_prefix_suffix_and_clip_like_make_array_type_name() {
        assert_eq!(array_type_name("mood", 0), "_mood");
        assert_eq!(array_type_name("mood", 1), "_mood_1");
        assert_eq!(array_type_name("_mood", 0), "__mood");
        let long = "a".repeat(63);
        assert_eq!(array_type_name(&long, 0), format!("_{}", "a".repeat(62)));
        assert_eq!(
            array_type_name(&long, 12),
            format!("_{}_12", "a".repeat(59))
        );
        let multibyte = format!("{}{}", "a".repeat(61), "\u{00e9}");
        assert_eq!(
            array_type_name(&multibyte, 0),
            format!("_{}", "a".repeat(61))
        );
        let taken = ["_x".to_owned(), "_x_1".to_owned()];
        assert_eq!(
            choose_array_type_name("x", |name| taken.iter().any(|taken| taken == name)),
            "_x_2"
        );
    }
}
