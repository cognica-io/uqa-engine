//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn bit_string_datums_keep_logical_length_owned_bounds_and_read_allowance() {
    let token = CancellationToken::new();
    for oid in [1560, 1562] {
        // Short varlena: one header byte, four length bytes, two packed bytes.
        let bytes = vec![15, 9, 0, 0, 0, 0x80, 0x80];
        let datum = DatumValue::new(oid, 0, bytes.clone());
        let memory = MemoryBudget::new(2);
        let control = ProductionControl::new(&memory, &token, &token);
        let value = read_with_control(&datum, &control).unwrap();
        assert_eq!(crate::expr::value_to_string(&value).unwrap(), "100000001");
        assert_eq!(memory.used(), 2);
        drop(value);
        assert_eq!(memory.used(), 0);
        let small = MemoryBudget::new(1);
        assert_eq!(
            read_with_control(&datum, &ProductionControl::new(&small, &token, &token))
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(small.used(), 0);
        for end in 0..bytes.len() {
            assert_eq!(
                read(&DatumValue::new(oid, 0, bytes[..end].to_vec()))
                    .unwrap_err()
                    .sqlstate(),
                Some("XX001")
            );
        }
        for invalid in [
            vec![15, 9, 0, 0, 0, 0x80, 0x81],
            vec![15, 17, 0, 0, 0, 0x80, 0x80],
            vec![15, 255, 255, 255, 255, 0x80, 0x80],
        ] {
            assert_eq!(
                read_with_control(&DatumValue::new(oid, 0, invalid), &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("XX001")
            );
            assert_eq!(memory.used(), 0);
        }
    }
}
