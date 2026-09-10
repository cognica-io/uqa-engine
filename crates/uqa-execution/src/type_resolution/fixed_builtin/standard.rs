//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric, text, and temporal fixed signatures used by ordinary SQL.

use super::{numeric_type, overload, BuiltinFunctionOverload, ColumnType};

pub(super) fn overloads(name: &str) -> Option<Vec<BuiltinFunctionOverload>> {
    use ColumnType as T;
    Some(match name {
        "round" | "trunc" => vec![
            overload(name, &[T::DoublePrecision], T::DoublePrecision),
            overload(name, &[numeric_type()], numeric_type()),
            overload(name, &[numeric_type(), T::Integer], numeric_type()),
        ],
        "substring" | "substr" => {
            let mut signatures = vec![
                overload(name, &[T::Text, T::Integer], T::Text),
                overload(name, &[T::Text, T::Integer, T::Integer], T::Text),
                overload(name, &[T::Bytea, T::Integer], T::Bytea),
                overload(name, &[T::Bytea, T::Integer, T::Integer], T::Bytea),
            ];
            if name == "substring" {
                signatures.extend([
                    overload(name, &[T::Text, T::Text], T::Text),
                    overload(name, &[T::Text, T::Text, T::Text], T::Text),
                ]);
            }
            signatures
        }
        "date_trunc" => vec![
            overload(name, &[T::Text, T::Timestamp], T::Timestamp),
            overload(name, &[T::Text, T::TimestampTz], T::TimestampTz),
            overload(name, &[T::Text, T::TimestampTz, T::Text], T::TimestampTz),
            overload(name, &[T::Text, T::Interval], T::Interval),
        ],
        "generate_series" => {
            let mut signatures = Vec::new();
            for ty in [T::Integer, T::BigInteger, numeric_type()] {
                signatures.push(overload(name, &[ty.clone(), ty.clone()], ty.clone()));
                signatures.push(overload(name, &[ty.clone(), ty.clone(), ty.clone()], ty));
            }
            for ty in [T::Timestamp, T::TimestampTz] {
                signatures.push(overload(name, &[ty.clone(), ty.clone(), T::Interval], ty));
            }
            signatures.push(overload(
                name,
                &[T::TimestampTz, T::TimestampTz, T::Interval, T::Text],
                T::TimestampTz,
            ));
            signatures
        }
        _ => return None,
    })
}
