//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer text input as `PostgreSQL`'s `int8in` reads it.

/// Why a text is not a 64-bit integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegerInputError {
    /// The text writes an integer outside the type's range.
    OutOfRange,
    /// The text does not write an integer.
    InvalidSyntax,
}

/// Read `text` as `PostgreSQL`'s `pg_strtoint64_safe` does: surrounding whitespace, an optional sign, and decimal digits or `0x`, `0o` and `0b` digits, which single underscores may separate.
pub(crate) fn parse_int8(text: &str) -> Result<i64, IntegerInputError> {
    let bytes = text.as_bytes();
    let mut position = 0;
    while bytes.get(position).is_some_and(|byte| is_space(*byte)) {
        position += 1;
    }
    let negative = match bytes.get(position) {
        Some(b'-') => {
            position += 1;
            true
        }
        Some(b'+') => {
            position += 1;
            false
        }
        _ => false,
    };
    let (radix, prefixed): (u32, bool) = match (bytes.get(position), bytes.get(position + 1)) {
        (Some(b'0'), Some(b'x' | b'X')) => (16, true),
        (Some(b'0'), Some(b'o' | b'O')) => (8, true),
        (Some(b'0'), Some(b'b' | b'B')) => (2, true),
        _ => (10, false),
    };
    if prefixed {
        position += 2;
    }
    let first_digit = position;
    // The magnitude of `i64::MIN` divided by the radix bounds a value that one more digit keeps in range.
    let limit = i64::MIN.unsigned_abs() / u64::from(radix);
    let mut magnitude: u64 = 0;
    while let Some(&byte) = bytes.get(position) {
        if let Some(digit) = char::from(byte).to_digit(radix) {
            if magnitude > limit {
                return Err(IntegerInputError::OutOfRange);
            }
            magnitude = magnitude * u64::from(radix) + u64::from(digit);
            position += 1;
        } else if byte == b'_' {
            // A decimal integer may not begin with an underscore, and every underscore separates two digits.
            if !prefixed && position == first_digit {
                return Err(IntegerInputError::InvalidSyntax);
            }
            position += 1;
            if !bytes
                .get(position)
                .is_some_and(|next| char::from(*next).is_digit(radix))
            {
                return Err(IntegerInputError::InvalidSyntax);
            }
        } else {
            break;
        }
    }
    if position == first_digit {
        return Err(IntegerInputError::InvalidSyntax);
    }
    while bytes.get(position).is_some_and(|byte| is_space(*byte)) {
        position += 1;
    }
    if position != bytes.len() {
        return Err(IntegerInputError::InvalidSyntax);
    }
    if negative {
        if magnitude > i64::MIN.unsigned_abs() {
            return Err(IntegerInputError::OutOfRange);
        }
        Ok(0_i64.wrapping_sub_unsigned(magnitude))
    } else {
        i64::try_from(magnitude).map_err(|_| IntegerInputError::OutOfRange)
    }
}

/// The bytes C's `isspace` accepts in the default locale.
const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

#[cfg(test)]
mod tests {
    use super::{parse_int8, IntegerInputError};

    #[test]
    fn reads_every_form_postgresql_accepts() {
        for (text, value) in [
            ("42", 42),
            ("  -42\n", -42),
            ("+7", 7),
            ("1_000_000", 1_000_000),
            ("0x1F", 31),
            ("0X_1f", 31),
            ("-0o17", -15),
            ("0b1010", 10),
            ("9223372036854775807", i64::MAX),
            ("-9223372036854775808", i64::MIN),
            ("-0x8000000000000000", i64::MIN),
        ] {
            assert_eq!(parse_int8(text), Ok(value), "{text}");
        }
    }

    #[test]
    fn distinguishes_an_out_of_range_integer_from_other_text() {
        for text in [
            "9223372036854775808",
            "-9223372036854775809",
            "0x8000000000000000",
            "99999999999999999999",
        ] {
            assert_eq!(
                parse_int8(text),
                Err(IntegerInputError::OutOfRange),
                "{text}"
            );
        }
        for text in [
            "", " ", "-", "1.5", "1e3", "_1", "1_", "1__0", "0x", "0x_", "12a", "0b2", "--1",
        ] {
            assert_eq!(
                parse_int8(text),
                Err(IntegerInputError::InvalidSyntax),
                "{text}"
            );
        }
    }
}
