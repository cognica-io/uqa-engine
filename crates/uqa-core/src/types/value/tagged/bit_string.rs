//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed bit tags retain exact length and reject noncanonical payloads.

use super::{decode_hex_bytes, int_field, BTreeMap, Value, ValueRetentionError, Workspace};
use crate::BitStringValue;

pub(super) fn decode(
    map: &BTreeMap<String, Value>,
    workspace: &mut Workspace<'_>,
) -> Result<BitStringValue, ValueRetentionError> {
    let malformed = || ValueRetentionError::Malformed {
        kind: "bit_string",
        reason: "invalid bit length, packed bytes or padding".into(),
    };
    if map.len() != 3 {
        return Err(malformed());
    }
    let length = int_field::<u32>(map, "bit_len").ok_or_else(malformed)?;
    let Some(Value::Str(hex)) = map.get("hex") else {
        return Err(malformed());
    };
    if hex.len() != length.div_ceil(8) as usize * 2 {
        return Err(malformed());
    }
    let bytes = decode_hex_bytes(hex, workspace)?.ok_or_else(malformed)?;
    BitStringValue::try_from_packed(bytes, length).ok_or_else(malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };

    #[test]
    fn bit_string_tag_roundtrips_nested_values_without_losing_length_or_payload_ownership() {
        let token = CancellationToken::new();
        for (bytes, length) in [
            (vec![], 0),
            (vec![0x80], 1),
            (vec![0x80], 8),
            (vec![0, 0x80], 9),
        ] {
            let bit =
                Value::BitString(BitStringValue::try_from_packed(bytes.clone(), length).unwrap());
            assert_ne!(bit, Value::Bytes(bytes));
            assert_ne!(bit, Value::Str("1".into()));
            for value in [
                bit.clone(),
                Value::List(vec![bit.clone()]),
                Value::Row(vec![bit.clone()].into()),
                Value::Record(vec![("bits".into(), bit.clone())].into()),
            ] {
                let json = serde_json::to_vec(&value).unwrap();
                let decoded: Value = serde_json::from_slice(&json).unwrap();
                assert!(value.has_same_representation(&decoded));
                let budget = MemoryBudget::new(32 * 1024);
                let control = ProductionControl::new(&budget, &token, &token);
                let copy = control.copy_value(&decoded).unwrap();
                assert!(copy.has_same_representation(&value));
                assert_eq!(
                    copy.reserved_bytes(),
                    copy.retained_payload_bytes(&budget, &token).unwrap()
                );
                drop(copy);
                assert_eq!(budget.used(), 0);
            }
        }
        let short: Value =
            serde_json::from_str(r#"{"$uqa_type":"bit_string","bit_len":1,"hex":"80"}"#).unwrap();
        let long: Value =
            serde_json::from_str(r#"{"$uqa_type":"bit_string","bit_len":8,"hex":"80"}"#).unwrap();
        assert!(short < long);
        let budget = MemoryBudget::new(0);
        let control = ProductionControl::new(&budget, &token, &token);
        assert!(short.cmp_with_control(&long, &control).unwrap().is_lt());
    }

    #[test]
    fn bit_string_tags_reject_noncanonical_data_in_controlled_and_ordinary_decoding() {
        for json in [
            r#"{"$uqa_type":"bit_string","bit_len":1,"hex":"81"}"#,
            r#"{"$uqa_type":"bit_string","bit_len":9,"hex":"80"}"#,
            r#"{"$uqa_type":"bit_string","bit_len":1,"hex":"8000"}"#,
            r#"{"$uqa_type":"bit_string","bit_len":-1,"hex":""}"#,
            r#"{"$uqa_type":"bit_string","bit_len":0,"hex":"00"}"#,
            r#"{"$uqa_type":"bit_string","bit_len":1,"hex":"zz"}"#,
            r#"{"$uqa_type":"bit_string","hex":"80"}"#,
        ] {
            assert!(serde_json::from_str::<Value>(json).is_err());
            let map: BTreeMap<String, Value> = serde_json::from_str(json).unwrap();
            let token = CancellationToken::new();
            let budget = MemoryBudget::new(32 * 1024);
            let bytes = Value::Map(map.clone())
                .retained_payload_bytes(&budget, &token)
                .unwrap();
            let input = crate::memory::Budgeted::new(map, budget.reserve(bytes).unwrap());
            assert!(super::super::value_from_tagged_map_budgeted(input, &token).is_err());
            assert_eq!(budget.used(), 0);
        }
    }
}
