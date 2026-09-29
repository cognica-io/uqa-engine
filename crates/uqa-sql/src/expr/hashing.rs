//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 hash primitives (`common/hashfn.c`) shared by hash partitioning and SQL hash support functions.

/// `hash_bytes_extended`: Jenkins lookup3 over a byte string with a 64-bit seed.
pub(crate) fn hash_bytes_extended(bytes: &[u8], seed: u64) -> u64 {
    let length = u32::try_from(bytes.len()).expect("SQL value length fits PostgreSQL's int width");
    let mut a = 0x9e37_79b9_u32.wrapping_add(length).wrapping_add(3_923_095);
    let mut b = a;
    let mut c = a;
    if seed != 0 {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        (a, b, c) = mix(a, b, c);
    }

    let mut chunks = bytes.chunks_exact(12);
    for chunk in &mut chunks {
        a = a.wrapping_add(u32::from_le_bytes(
            chunk[0..4].try_into().expect("chunk width"),
        ));
        b = b.wrapping_add(u32::from_le_bytes(
            chunk[4..8].try_into().expect("chunk width"),
        ));
        c = c.wrapping_add(u32::from_le_bytes(
            chunk[8..12].try_into().expect("chunk width"),
        ));
        (a, b, c) = mix(a, b, c);
    }
    let tail = chunks.remainder();
    for (index, byte) in tail.iter().take(4).enumerate() {
        a = a.wrapping_add(u32::from(*byte) << (index * 8));
    }
    for (index, byte) in tail.iter().skip(4).take(4).enumerate() {
        b = b.wrapping_add(u32::from(*byte) << (index * 8));
    }
    for (index, byte) in tail.iter().skip(8).enumerate() {
        c = c.wrapping_add(u32::from(*byte) << ((index + 1) * 8));
    }
    let (_, b, c) = final_mix(a, b, c);
    (u64::from(b) << 32) | u64::from(c)
}

/// `hash_bytes_uint32_extended`: the same hash over one 32-bit word. With seed 0 its low half equals `hash_bytes_uint32`, the 32-bit hash of `hash_uint32`.
pub(crate) fn hash_bytes_uint32_extended(value: u32, seed: u64) -> u64 {
    let mut a = 0x9e37_79b9_u32
        .wrapping_add(u32::try_from(std::mem::size_of::<u32>()).expect("u32 width"))
        .wrapping_add(3_923_095);
    let mut b = a;
    let mut c = a;
    if seed != 0 {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        (a, b, c) = mix(a, b, c);
    }
    a = a.wrapping_add(value);
    let (_, b, c) = final_mix(a, b, c);
    (u64::from(b) << 32) | u64::from(c)
}

fn mix(mut a: u32, mut b: u32, mut c: u32) -> (u32, u32, u32) {
    a = a.wrapping_sub(c);
    a ^= c.rotate_left(4);
    c = c.wrapping_add(b);
    b = b.wrapping_sub(a);
    b ^= a.rotate_left(6);
    a = a.wrapping_add(c);
    c = c.wrapping_sub(b);
    c ^= b.rotate_left(8);
    b = b.wrapping_add(a);
    a = a.wrapping_sub(c);
    a ^= c.rotate_left(16);
    c = c.wrapping_add(b);
    b = b.wrapping_sub(a);
    b ^= a.rotate_left(19);
    a = a.wrapping_add(c);
    c = c.wrapping_sub(b);
    c ^= b.rotate_left(4);
    b = b.wrapping_add(a);
    (a, b, c)
}

fn final_mix(mut a: u32, mut b: u32, mut c: u32) -> (u32, u32, u32) {
    c ^= b;
    c = c.wrapping_sub(b.rotate_left(14));
    a ^= c;
    a = a.wrapping_sub(c.rotate_left(11));
    b ^= a;
    b = b.wrapping_sub(a.rotate_left(25));
    c ^= b;
    c = c.wrapping_sub(b.rotate_left(16));
    a ^= c;
    a = a.wrapping_sub(c.rotate_left(4));
    b ^= a;
    b = b.wrapping_sub(a.rotate_left(14));
    c ^= b;
    c = c.wrapping_sub(b.rotate_left(24));
    (a, b, c)
}
