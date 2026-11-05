//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn vector_record_windows_preserve_all_float_bits_without_decoding_spilled_payloads() {
    let bits = [
        0,
        0x8000_0000,
        0x7f7f_ffff,
        0xff7f_ffff,
        0x7fc0_0001,
        0x7f80_0000,
    ];
    for resident in [0, 1 << 20] {
        let source = StorageReadControl::with_limit(4 << 20);
        let mut map = Map::new(source.memory(), resident);
        map.insert(
            7,
            HNSWVector {
                values: (0..6000)
                    .map(|i| f32::from_bits(bits[i % bits.len()]))
                    .collect(),
                norm: f32::from_bits(0x7fc0_0042),
            },
            Some(&source),
        )
        .unwrap();
        let invoking = StorageReadControl::with_limit(0);
        crate::hnsw_index::store::DECODED_VECTOR_FLOATS.set(0);
        let vector = VectorRead::open(&map, 7, &invoking).unwrap().unwrap();
        assert_eq!(vector.len(), 6000);
        assert_eq!(vector.norm().to_bits(), 0x7fc0_0042);
        let held = source
            .memory()
            .reserve(source.memory().available())
            .unwrap();
        let mut output = [0_f32; 2048];
        let allocation = allocation_counter::measure(|| {
            for width in [1, 7, 1024, 2048] {
                let mut offset = 0;
                while offset < vector.len() {
                    let count = vector
                        .read_values(offset, &mut output[..width], &invoking)
                        .unwrap();
                    assert_eq!(count, width.min(1024).min(vector.len() - offset));
                    for (index, value) in output[..count].iter().enumerate() {
                        assert_eq!(value.to_bits(), bits[(offset + index) % bits.len()]);
                    }
                    offset += count;
                }
                assert_eq!(
                    vector.read_values(offset, &mut output, &invoking).unwrap(),
                    0
                );
            }
        });
        assert_eq!(allocation.bytes_total, 0);
        assert_eq!(crate::hnsw_index::store::DECODED_VECTOR_FLOATS.get(), 0);
        assert!(vector
            .read_values(vector.len() + 1, &mut output, &invoking)
            .is_err());
        invoking.cancellation().cancel();
        assert!(vector.read_values(0, &mut [], &invoking).is_err());
        invoking.cancellation().reset();
        assert!(VectorRead::open(&map, 8, &invoking).unwrap().is_none());
        drop((held, map));
        assert_eq!(source.memory().used(), 0);
    }
}
