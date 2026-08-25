//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18-compatible hash-partition support functions.

use uqa_core::{TemporalValue, Value};
use uqa_sql::ast::{ColumnDef, ColumnType, Expr, PartitionSpec, PartitionStrategy};
use uqa_sql::SQLError;

use crate::Engine;

const HASH_PARTITION_SEED: u64 = 0x7a5b_2236_7996_dcfd;
const HASH_COMBINE_CONSTANT: u64 = 0x49a0_f4dd_15e5_a8e3;
const POSTGRES_EPOCH_UNIX_DAYS: i32 = 10_957;

pub(super) fn validate_partition_spec(
    engine: &Engine,
    spec: &PartitionSpec,
    columns: &[ColumnDef],
) -> Result<(), SQLError> {
    if spec.strategy != PartitionStrategy::Hash {
        return Ok(());
    }
    for key in &spec.keys {
        let ty = partition_key_type(engine, key, columns)?;
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

pub(super) fn validate_modulus_chain(
    new_modulus: i32,
    existing_moduli: impl IntoIterator<Item = i32>,
) -> Result<(), SQLError> {
    let mut moduli = existing_moduli.into_iter().collect::<Vec<_>>();
    moduli.push(new_modulus);
    moduli.sort_unstable();
    moduli.dedup();
    if moduli
        .windows(2)
        .any(|pair| pair[1].rem_euclid(pair[0]) != 0)
    {
        return Err(invalid_partition_bound(
            "every hash partition modulus must be a factor of the next larger modulus",
        ));
    }
    Ok(())
}

pub(super) fn bounds_overlap(
    left_modulus: i32,
    left_remainder: i32,
    right_modulus: i32,
    right_remainder: i32,
) -> Result<bool, SQLError> {
    validate_bound(left_modulus, left_remainder)?;
    validate_bound(right_modulus, right_remainder)?;
    Ok((left_remainder - right_remainder)
        .rem_euclid(greatest_common_divisor(left_modulus, right_modulus))
        == 0)
}

pub(super) fn row_hash(
    engine: &Engine,
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
    let mut row_hash = 0_u64;
    for (key, value) in spec.keys.iter().zip(values) {
        let ty = partition_key_type(engine, key, columns)?;
        if matches!(value, Value::Null) {
            continue;
        }
        row_hash = hash_combine64(row_hash, hash_value(value, &ty)?);
    }
    Ok(row_hash)
}

pub(super) fn bound_matches(row_hash: u64, modulus: i32, remainder: i32) -> Result<bool, SQLError> {
    validate_bound(modulus, remainder)?;
    let modulus = u64::try_from(modulus).expect("validated HASH modulus is positive");
    let remainder = u64::try_from(remainder).expect("validated HASH remainder is nonnegative");
    Ok(row_hash % modulus == remainder)
}

fn partition_key_type(
    engine: &Engine,
    expression: &Expr,
    columns: &[ColumnDef],
) -> Result<ColumnType, SQLError> {
    if let Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } = expression {
        return columns
            .iter()
            .find(|column| column.name == *name)
            .map(|column| column.ty.clone())
            .ok_or_else(|| SQLError::UnknownColumn(name.clone()));
    }
    let schema = uqa_execution::RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let expression = uqa_planner::ExpressionPlan::lower(expression.clone());
    uqa_execution::common_context_expression_type(&expression.scalar, &schema, &[], Some(engine))?
        .ok_or_else(|| SQLError::TypeMismatch("cannot determine HASH partition key type".into()))
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
        | ColumnType::Date => Ok(()),
        ColumnType::Domain { base, .. } => validate_partition_key_type(base),
        other => Err(SQLError::Unsupported(format!(
            "HASH partition key type `{}` is not supported",
            other.sql_name()
        ))),
    }
}

fn hash_value(value: &Value, ty: &ColumnType) -> Result<u64, SQLError> {
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
            let bytes = uqa_sql::expr::parse_uuid_bytes(text)?;
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
        ColumnType::Domain { base, .. } => hash_value(value, base),
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

fn hash_bytes_extended(bytes: &[u8], seed: u64) -> u64 {
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

fn hash_bytes_uint32_extended(value: u32, seed: u64) -> u64 {
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

fn hash_combine64(left: u64, right: u64) -> u64 {
    left ^ right
        .wrapping_add(HASH_COMBINE_CONSTANT)
        .wrapping_add(left << 54)
        .wrapping_add(left >> 7)
}

const fn greatest_common_divisor(mut left: i32, mut right: i32) -> i32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.abs()
}

fn invalid_table_definition(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P16".into(),
        message: message.into(),
    }
}

fn invalid_partition_bound(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_extended_hash_vectors_match() {
        assert_eq!(
            hash_value(&Value::Int(-1), &ColumnType::SmallInteger).unwrap(),
            -5_017_072_347_659_237_694_i64 as u64
        );
        assert_eq!(
            hash_value(&Value::Int(i64::MIN), &ColumnType::BigInteger).unwrap(),
            -6_050_265_599_104_649_060_i64 as u64
        );
        assert_eq!(
            hash_value(&Value::Str("alpha".into()), &ColumnType::Text).unwrap(),
            5_995_266_089_327_636_298_u64
        );
        assert_eq!(
            hash_value(&Value::Str("한글".into()), &ColumnType::Text).unwrap(),
            -955_099_021_262_996_613_i64 as u64
        );
        assert_eq!(
            hash_value(
                &Value::Str("550e8400-e29b-41d4-a716-446655440000".into()),
                &ColumnType::Uuid,
            )
            .unwrap(),
            -3_467_891_652_331_307_802_i64 as u64
        );
        assert_eq!(
            hash_value(
                &Value::Temporal(TemporalValue::Date { days: 0 }),
                &ColumnType::Date,
            )
            .unwrap(),
            -7_791_128_061_482_025_433_i64 as u64
        );
    }

    #[test]
    fn postgres_modulo_seventeen_and_domain_vectors_match() {
        let remainder =
            |value: &Value, ty: &ColumnType| hash_combine64(0, hash_value(value, ty).unwrap()) % 17;
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
            hash_combine64(hash, hash_value(value, ty).unwrap())
        });
        assert_eq!(hash % 17, 11);
    }
}
