//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hashing and the text formats of bytea values for scalar functions and the `bytea` input function.

use crate::error::{Result, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

// -------------------------------------------------------------------------
// MD5 implementation used by the SQL scalar `md5()` builtin.
// -------------------------------------------------------------------------

#[cfg(test)]
pub(super) fn md5_hex(input: &[u8]) -> String {
    md5_hex_with_control(input, &ProductionControl::uncontrolled())
        .expect("ordinary MD5 encoding")
        .into_uncontrolled()
        .expect("ordinary MD5 owner")
}

pub(super) fn md5_hex_with_control(
    input: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let digest = md5_compute(input, control)?;
    hex_encode_with_control(&digest, control)
}

#[allow(dead_code, clippy::many_single_char_names)]
#[expect(
    clippy::too_many_lines,
    reason = "canonical encoding keeps every SQL value tag in one dispatch"
)]
fn md5_compute(input: &[u8], control: &ProductionControl<'_>) -> Result<[u8; 16]> {
    control.check()?;
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcef_a3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];

    let mut a0: u32 = 0x6745_2301;
    let mut b0: u32 = 0xefcd_ab89;
    let mut c0: u32 = 0x98ba_dcfe;
    let mut d0: u32 = 0x1032_5476;

    // Full input blocks are borrowed. Only the final one or two padded blocks need storage.
    let (chunks, remaining) = input.as_chunks::<64>();
    let mut tail = [0_u8; 128];
    tail[..remaining.len()].copy_from_slice(remaining);
    tail[remaining.len()] = 0x80;
    let tail_len = if remaining.len() < 56 { 64 } else { 128 };
    let bits = (input.len() as u64).wrapping_mul(8);
    tail[tail_len - 8..tail_len].copy_from_slice(&bits.to_le_bytes());
    for chunk in chunks.iter().chain(tail[..tail_len].as_chunks::<64>().0) {
        control.check()?;
        let mut m = [0u32; 16];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    Ok(out)
}

// -------------------------------------------------------------------------
// The text formats of bytea values, as PostgreSQL's `encode.c` writes and reads them for `encode`, `decode` and the `bytea` input function.
// -------------------------------------------------------------------------

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The output line length of the base64 format, after which `pg_base64_encode` starts a new line.
const BASE64_LINE: usize = 76;

#[cfg(test)]
pub(super) fn base64_encode(input: &[u8]) -> String {
    base64_encode_with_control(input, &ProductionControl::uncontrolled())
        .expect("ordinary base64 encoding")
        .into_uncontrolled()
        .expect("ordinary base64 owner")
}

/// The base64 format of `input` as `pg_base64_encode` writes it: a line break after every 76 characters, including one that ends the output.
pub(super) fn base64_encode_with_control(
    input: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let encoded = input
        .len()
        .div_ceil(3)
        .checked_mul(4)
        .ok_or_else(|| super::allocation_error("base64 encode"))?;
    let mut out = ProductionString::new(*control);
    out.reserve(encoded + encoded / BASE64_LINE)?;
    let mut line = 0;
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(BASE64_ALPHABET[(b0 >> 2) as usize] as char)?;
        out.push(BASE64_ALPHABET[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char)?;
        if chunk.len() > 1 {
            out.push(BASE64_ALPHABET[(((b1 & 0b1111) << 2) | (b2 >> 6)) as usize] as char)?;
        } else {
            out.push('=')?;
        }
        if chunk.len() > 2 {
            out.push(BASE64_ALPHABET[(b2 & 0b11_1111) as usize] as char)?;
            line += 4;
            if line >= BASE64_LINE {
                out.push('\n')?;
                line = 0;
            }
        } else {
            out.push('=')?;
        }
    }
    Ok(out.finish()?)
}

#[cfg(test)]
pub(super) fn base64_decode(input: &str) -> Result<Vec<u8>> {
    base64_decode_with_control(input, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary base64 owner".into()))
}

/// Read the base64 format as `pg_base64_decode` does: whitespace is skipped, `=` pads the last group of a sequence, and a group left incomplete fails.
pub(super) fn base64_decode_with_control(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>> {
    let mut decoded = ProductionVec::new(*control);
    decoded.reserve(input.len() / 4 * 3)?;
    let mut buffer = 0_u32;
    let mut position = 0;
    // The padding that ended a sequence: 1 after two symbols, 2 after three. `PostgreSQL` keeps it for every later group.
    let mut end = 0;
    for symbol in input.chars() {
        control.check()?;
        if matches!(symbol, ' ' | '\t' | '\n' | '\r') {
            continue;
        }
        let value = if symbol == '=' {
            if end == 0 {
                end = match position {
                    2 => 1,
                    3 => 2,
                    _ => {
                        return Err(invalid_encoding(
                            "unexpected \"=\" while decoding base64 sequence".into(),
                        ))
                    }
                };
            }
            0
        } else {
            u8::try_from(symbol)
                .ok()
                .and_then(|byte| BASE64_ALPHABET.iter().position(|letter| *letter == byte))
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    invalid_encoding(format!(
                        "invalid symbol \"{symbol}\" found while decoding base64 sequence"
                    ))
                })?
        };
        buffer = (buffer << 6) + value;
        position += 1;
        if position == 4 {
            let [_, first, second, third] = buffer.to_be_bytes();
            decoded.push_copy(first)?;
            if end == 0 || end > 1 {
                decoded.push_copy(second)?;
            }
            if end == 0 || end > 2 {
                decoded.push_copy(third)?;
            }
            buffer = 0;
            position = 0;
        }
    }
    if position != 0 {
        return Err(SQLError::Diagnostic {
            sqlstate: "22023".into(),
            message: "invalid base64 end sequence".into(),
            detail: None,
            hint: Some(
                "Input data is missing padding, is truncated, or is otherwise corrupted.".into(),
            ),
        });
    }
    Ok(decoded.finish()?)
}

/// Read the hex format as `hex_decode_safe` does: pairs of hex digits, which spaces, tabs and line breaks may separate.
pub(crate) fn hex_decode_with_control(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>> {
    let mut decoded = ProductionVec::new(*control);
    decoded.reserve(input.len() / 2)?;
    let mut digits = input.chars();
    while let Some(high) = digits.next() {
        control.check()?;
        if matches!(high, ' ' | '\n' | '\t' | '\r') {
            continue;
        }
        let high = hex_digit(high)?;
        let low = digits.next().ok_or_else(|| {
            invalid_encoding("invalid hexadecimal data: odd number of digits".into())
        })?;
        decoded.push_copy(high * 16 + hex_digit(low)?)?;
    }
    Ok(decoded.finish()?)
}

fn hex_digit(digit: char) -> Result<u8> {
    digit
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| invalid_encoding(format!("invalid hexadecimal digit: \"{digit}\"")))
}

/// The escape format of `input` as `esc_enc` writes it: a NUL or a byte with its high bit set as a backslash and three octal digits, a backslash doubled, and every other byte as it is.
pub(super) fn escape_encode_with_control(
    input: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut encoded = ProductionString::new(*control);
    encoded.reserve(input.len())?;
    for &byte in input {
        control.check()?;
        if byte == 0 || byte & 0x80 != 0 {
            for digit in [
                b'\\',
                b'0' + (byte >> 6),
                b'0' + ((byte >> 3) & 7),
                b'0' + (byte & 7),
            ] {
                encoded.push(char::from(digit))?;
            }
        } else if byte == b'\\' {
            encoded.push('\\')?;
            encoded.push('\\')?;
        } else {
            encoded.push(char::from(byte))?;
        }
    }
    Ok(encoded.finish()?)
}

/// Read the escape format as `esc_dec` and the `bytea` input function do: a backslash introduces another backslash or three octal digits, the first of them at most 3.
pub(crate) fn escape_decode_with_control(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>> {
    let input = input.as_bytes();
    let mut decoded = ProductionVec::new(*control);
    decoded.reserve(input.len())?;
    let mut index = 0;
    while index < input.len() {
        control.check()?;
        if input[index] != b'\\' {
            decoded.push_copy(input[index])?;
            index += 1;
            continue;
        }
        if let Some([first @ b'0'..=b'3', second @ b'0'..=b'7', third @ b'0'..=b'7']) =
            input.get(index + 1..index + 4)
        {
            decoded.push_copy((first - b'0') * 64 + (second - b'0') * 8 + (third - b'0'))?;
            index += 4;
        } else if input.get(index + 1) == Some(&b'\\') {
            decoded.push_copy(b'\\')?;
            index += 2;
        } else {
            return Err(SQLError::Routine {
                sqlstate: "22P02".into(),
                message: "invalid input syntax for type bytea".into(),
            });
        }
    }
    Ok(decoded.finish()?)
}

fn invalid_encoding(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message,
    }
}

pub(super) fn hex_encode_with_control(
    bytes: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let capacity = bytes
        .len()
        .checked_mul(2)
        .ok_or_else(|| super::allocation_error("hex encode"))?;
    let mut out = ProductionString::new(*control);
    out.reserve(capacity)?;
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char)?;
        out.push(HEX[usize::from(byte & 15)] as char)?;
    }
    Ok(out.finish()?)
}

#[cfg(test)]
mod production_tests;
