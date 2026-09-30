//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18-compatible hash-partition support functions.

use super::PartitionContext;
use crate::ast::{ColumnDef, ColumnType, PartitionSpec, PartitionStrategy};
use crate::expr::enums::EnumLabelCatalog;
use crate::SQLError;
use uqa_core::{TemporalValue, Value};

use super::key::key_type;
use crate::expr::hashing::{hash_bytes_extended, hash_bytes_uint32_extended};
use crate::type_resolution::FunctionTypeResolver;

const HASH_PARTITION_SEED: u64 = 0x7a5b_2236_7996_dcfd;
const HASH_COMBINE_CONSTANT: u64 = 0x49a0_f4dd_15e5_a8e3;
const POSTGRES_EPOCH_UNIX_DAYS: i32 = 10_957;

pub(super) fn validate_partition_spec(
    resolver: &dyn FunctionTypeResolver,
    spec: &PartitionSpec,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    if spec.strategy != PartitionStrategy::Hash {
        return Ok(());
    }
    for key in &spec.keys {
        let ty = key_type(resolver, key, columns)?;
        validate_partition_key_type(&ty)?;
    }
    Ok(())
}

pub(super) fn validate_bound(modulus: i32, remainder: i32) -> Result<(), SQLError> {
    if modulus <= 0 {
        return Err(invalid_table_definition(
            "modulus for hash partition must be an integer value greater than zero",
        ));
    }
    if remainder < 0 {
        return Err(invalid_table_definition(
            "remainder for hash partition must be an integer value greater than or equal to zero",
        ));
    }
    if remainder >= modulus {
        return Err(invalid_table_definition(
            "remainder for hash partition must be less than modulus",
        ));
    }
    Ok(())
}

/// `compute_partition_hash_value`: combine the extended hash of every non-NULL key with the partition seed.
pub(super) fn row_hash(
    context: &PartitionContext<'_>,
    spec: &PartitionSpec,
    columns: &[ColumnDef],
    values: &[Value],
) -> Result<u64, SQLError> {
    if spec.keys.len() != values.len() {
        return Err(SQLError::Internal(format!(
            "HASH partition key width {} differs from row key width {}",
            spec.keys.len(),
            values.len()
        )));
    }
    let labels = context.types.enum_labels();
    let mut row_hash = 0_u64;
    for (key, value) in spec.keys.iter().zip(values) {
        if matches!(value, Value::Null) {
            continue;
        }
        let ty = key_type(context.types, key, columns)?;
        row_hash = hash_combine64(row_hash, hash_value(value, &ty, labels)?);
    }
    Ok(row_hash)
}

pub(super) fn bound_matches(row_hash: u64, modulus: i32, remainder: i32) -> Result<bool, SQLError> {
    validate_bound(modulus, remainder)?;
    let modulus = u64::try_from(modulus).expect("validated HASH modulus is positive");
    let remainder = u64::try_from(remainder).expect("validated HASH remainder is nonnegative");
    Ok(row_hash % modulus == remainder)
}

fn validate_partition_key_type(ty: &ColumnType) -> Result<(), SQLError> {
    match ty {
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Text
        | ColumnType::Name
        | ColumnType::Uuid
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Date
        | ColumnType::Enum(_) => Ok(()),
        ColumnType::Domain { base, .. } => validate_partition_key_type(base),
        other => Err(SQLError::Unsupported(format!(
            "HASH partition key type `{}` is not supported",
            other.sql_name()
        ))),
    }
}

fn hash_value(
    value: &Value,
    ty: &ColumnType,
    labels: Option<&dyn EnumLabelCatalog>,
) -> Result<u64, SQLError> {
    match ty {
        ColumnType::SmallInteger => {
            let value = i16::try_from(integer_value(value, ty)?)
                .map_err(|_| hash_value_type_mismatch(value, ty))?;
            Ok(hash_bytes_uint32_extended(
                i32::from(value) as u32,
                HASH_PARTITION_SEED,
            ))
        }
        ColumnType::Integer => {
            let value = i32::try_from(integer_value(value, ty)?)
                .map_err(|_| hash_value_type_mismatch(value, ty))?;
            Ok(hash_bytes_uint32_extended(
                value as u32,
                HASH_PARTITION_SEED,
            ))
        }
        ColumnType::BigInteger => {
            let value = integer_value(value, ty)?;
            let low = value as u32;
            let high = (value >> 32) as u32;
            let folded = low ^ if value >= 0 { high } else { !high };
            Ok(hash_bytes_uint32_extended(folded, HASH_PARTITION_SEED))
        }
        ColumnType::Text | ColumnType::Name | ColumnType::Varchar(_) => Ok(hash_bytes_extended(
            string_value(value, ty)?.as_bytes(),
            HASH_PARTITION_SEED,
        )),
        ColumnType::Bpchar | ColumnType::Character(_) => {
            let bytes = string_value(value, ty)?.as_bytes();
            let significant = bytes
                .iter()
                .rposition(|byte| *byte != b' ')
                .map_or(0, |index| index + 1);
            Ok(hash_bytes_extended(
                &bytes[..significant],
                HASH_PARTITION_SEED,
            ))
        }
        ColumnType::Uuid => {
            let text = string_value(value, ty)?;
            let bytes = crate::expr::parse_uuid_bytes(text)?;
            Ok(hash_bytes_extended(&bytes, HASH_PARTITION_SEED))
        }
        ColumnType::Date => {
            let Value::Temporal(TemporalValue::Date { days }) = value else {
                return Err(hash_value_type_mismatch(value, ty));
            };
            let postgres_days = days
                .checked_sub(POSTGRES_EPOCH_UNIX_DAYS)
                .ok_or_else(|| hash_value_type_mismatch(value, ty))?;
            Ok(hash_bytes_uint32_extended(
                postgres_days as u32,
                HASH_PARTITION_SEED,
            ))
        }
        // `hashenumextended` hashes the label OID, which the catalog assigns and renames never change.
        ColumnType::Enum(_) => {
            let Value::Enum(label) = value else {
                return Err(hash_value_type_mismatch(value, ty));
            };
            let oid = crate::expr::enums::enum_label(labels, label)?.oid;
            Ok(hash_bytes_uint32_extended(oid, HASH_PARTITION_SEED))
        }
        ColumnType::Domain { base, .. } => hash_value(value, base, labels),
        other => Err(SQLError::Unsupported(format!(
            "HASH partition key type `{}` is not supported",
            other.sql_name()
        ))),
    }
}

fn integer_value(value: &Value, ty: &ColumnType) -> Result<i64, SQLError> {
    match value {
        Value::Int(value) => Ok(*value),
        _ => Err(hash_value_type_mismatch(value, ty)),
    }
}

fn string_value<'a>(value: &'a Value, ty: &ColumnType) -> Result<&'a str, SQLError> {
    match value {
        Value::Str(value) | Value::FixedChar(value) => Ok(value),
        _ => Err(hash_value_type_mismatch(value, ty)),
    }
}

fn hash_value_type_mismatch(value: &Value, ty: &ColumnType) -> SQLError {
    SQLError::TypeMismatch(format!(
        "HASH partition key value {value:?} does not match type `{}`",
        ty.sql_name()
    ))
}

fn hash_combine64(left: u64, right: u64) -> u64 {
    left ^ right
        .wrapping_add(HASH_COMBINE_CONSTANT)
        .wrapping_add(left << 54)
        .wrapping_add(left >> 7)
}

fn invalid_table_definition(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P16".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_extended_hash_vectors_match() {
        assert_eq!(
            hash_value(&Value::Int(-1), &ColumnType::SmallInteger, None).unwrap(),
            -5_017_072_347_659_237_694_i64 as u64
        );
        assert_eq!(
            hash_value(&Value::Int(i64::MIN), &ColumnType::BigInteger, None).unwrap(),
            -6_050_265_599_104_649_060_i64 as u64
        );
        assert_eq!(
            hash_value(&Value::Str("alpha".into()), &ColumnType::Text, None).unwrap(),
            5_995_266_089_327_636_298_u64
        );
        assert_eq!(
            hash_value(
                &Value::Str("\u{d55c}\u{ae00}".into()),
                &ColumnType::Text,
                None
            )
            .unwrap(),
            -955_099_021_262_996_613_i64 as u64
        );
        assert_eq!(
            hash_value(
                &Value::Str("550e8400-e29b-41d4-a716-446655440000".into()),
                &ColumnType::Uuid,
                None
            )
            .unwrap(),
            -3_467_891_652_331_307_802_i64 as u64
        );
        assert_eq!(
            hash_value(
                &Value::Temporal(TemporalValue::Date { days: 0 }),
                &ColumnType::Date,
                None
            )
            .unwrap(),
            -7_791_128_061_482_025_433_i64 as u64
        );
    }

    #[test]
    fn postgres_modulo_seventeen_and_domain_vectors_match() {
        let remainder = |value: &Value, ty: &ColumnType| {
            hash_combine64(0, hash_value(value, ty, None).unwrap()) % 17
        };
        assert_eq!(remainder(&Value::Int(0), &ColumnType::BigInteger), 10);
        assert_eq!(remainder(&Value::Int(1), &ColumnType::BigInteger), 7);
        assert_eq!(remainder(&Value::Int(-1), &ColumnType::BigInteger), 13);
        assert_eq!(remainder(&Value::Str("alpha".into()), &ColumnType::Text), 4);
        assert_eq!(
            remainder(
                &Value::Str("550e8400-e29b-41d4-a716-446655440000".into()),
                &ColumnType::Uuid,
            ),
            11
        );
        assert_eq!(
            remainder(
                &Value::Temporal(TemporalValue::Date { days: 0 }),
                &ColumnType::Date,
            ),
            5
        );
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "positive_integer".into(),
            oid: 42,
            array_oid: None,
            base: Box::new(ColumnType::Integer),
        };
        assert_eq!(remainder(&Value::Int(42), &domain), 14);
        let composite = [
            (&Value::Int(1), &ColumnType::Integer),
            (&Value::Str("alpha".into()), &ColumnType::Text),
            (
                &Value::Str("550e8400-e29b-41d4-a716-446655440000".into()),
                &ColumnType::Uuid,
            ),
        ];
        let hash = composite.iter().fold(0_u64, |hash, (value, ty)| {
            hash_combine64(hash, hash_value(value, ty, None).unwrap())
        });
        assert_eq!(hash % 17, 11);
    }
}
