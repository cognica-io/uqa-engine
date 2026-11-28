//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{hash_magnitude, Hasher, MAX_FRACTIONAL_DIGITS, RADIX};
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};

#[derive(Default)]
struct Digits {
    limbs: Vec<u64>,
    integer_len: usize,
    fraction_len: usize,
}

impl Hasher for Digits {
    fn finish(&self) -> u64 {
        unreachable!("the test reads the full hash input")
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("numeric magnitude emits typed limbs and lengths")
    }

    fn write_u64(&mut self, limb: u64) {
        self.limbs.push(limb);
    }

    fn write_u32(&mut self, length: u32) {
        self.integer_len = length as usize;
    }

    fn write_usize(&mut self, length: usize) {
        self.fraction_len = length;
    }
}

#[test]
fn hash_input_matches_independent_arbitrary_precision_quotients_and_fractional_digits() {
    let five = BigUint::from(5_u8);
    let radix = BigUint::from(RADIX);
    let values = [
        BigUint::from(7_u8),
        (BigUint::from(1_u8) << 193_usize) + 19_u8,
        five.pow(81) * ((BigUint::from(1_u8) << 319_usize) + 1_u8),
    ];
    for scale in [
        0,
        1,
        26,
        27,
        28,
        53,
        54,
        55,
        81,
        1080,
        MAX_FRACTIONAL_DIGITS,
    ] {
        let denominator = five.pow(scale);
        for value in &values {
            let mut actual = Digits::default();
            hash_magnitude(value.iter_u64_digits().rev(), scale, &mut actual);
            let integer: Vec<_> = (value / &denominator).iter_u64_digits().rev().collect();
            let mut remainder = value % &denominator;
            let mut fraction = Vec::new();
            while !remainder.is_zero() {
                remainder *= &radix;
                fraction.push((&remainder / &denominator).to_u64().unwrap());
                remainder %= &denominator;
            }
            assert_eq!(actual.integer_len, integer.len(), "scale {scale}");
            assert_eq!(actual.fraction_len, fraction.len(), "scale {scale}");
            assert_eq!(actual.limbs[..actual.integer_len], integer);
            assert_eq!(actual.limbs[actual.integer_len..], fraction);
        }
    }
}
