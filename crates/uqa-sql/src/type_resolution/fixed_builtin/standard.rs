//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric, text, and temporal fixed signatures used by ordinary SQL.

use super::registry::{
    declarations, numeric_type, Signature, NUMERIC_BINARY_ARGUMENTS, NUMERIC_SCALE_ARGUMENTS,
    NUMERIC_TERNARY_ARGUMENTS, NUMERIC_UNARY_ARGUMENTS,
};
use crate::ast::ColumnType as T;

declarations! { pub(super) fn lookup(name);
        "mod" => &[
            Signature::new(&[T::SmallInteger, T::SmallInteger], T::SmallInteger),
            Signature::new(&[T::Integer, T::Integer], T::Integer),
            Signature::new(&[T::BigInteger, T::BigInteger], T::BigInteger),
            Signature::new(&NUMERIC_BINARY_ARGUMENTS, numeric_type()),
        ],
        "power" | "pow" => &[
            Signature::new(&[T::DoublePrecision, T::DoublePrecision], T::DoublePrecision),
            Signature::new(&NUMERIC_BINARY_ARGUMENTS, numeric_type()),
        ],
        "sqrt" => &[
            Signature::new(&[T::DoublePrecision], T::DoublePrecision),
            Signature::new(&NUMERIC_UNARY_ARGUMENTS, numeric_type()),
        ],
        "cbrt" => &[Signature::new(&[T::DoublePrecision], T::DoublePrecision)],
        "round" | "trunc" => &[
            Signature::new(&[T::DoublePrecision], T::DoublePrecision),
            Signature::new(&NUMERIC_UNARY_ARGUMENTS, numeric_type()),
            Signature::new(&NUMERIC_SCALE_ARGUMENTS, numeric_type()),
        ],
        "substr" => &[
            Signature::new(&[T::Text, T::Integer], T::Text),
            Signature::new(&[T::Text, T::Integer, T::Integer], T::Text),
            Signature::new(&[T::Bytea, T::Integer], T::Bytea),
            Signature::new(&[T::Bytea, T::Integer, T::Integer], T::Bytea),
        ],
        "substring" => &[
            Signature::new(&[T::Text, T::Integer], T::Text),
            Signature::new(&[T::Text, T::Integer, T::Integer], T::Text),
            Signature::new(&[T::Bytea, T::Integer], T::Bytea),
            Signature::new(&[T::Bytea, T::Integer, T::Integer], T::Bytea),
            Signature::new(&[T::Text, T::Text], T::Text),
            Signature::new(&[T::Text, T::Text, T::Text], T::Text),
        ],
        "date_trunc" => &[
            Signature::new(&[T::Text, T::Timestamp], T::Timestamp),
            Signature::new(&[T::Text, T::TimestampTz], T::TimestampTz),
            Signature::new(&[T::Text, T::TimestampTz, T::Text], T::TimestampTz),
            Signature::new(&[T::Text, T::Interval], T::Interval),
        ],
        "generate_series" => &[
            Signature::new(&[T::Integer, T::Integer], T::Integer),
            Signature::new(&[T::Integer, T::Integer, T::Integer], T::Integer),
            Signature::new(&[T::BigInteger, T::BigInteger], T::BigInteger),
            Signature::new(&[T::BigInteger, T::BigInteger, T::BigInteger], T::BigInteger),
            Signature::new(&NUMERIC_BINARY_ARGUMENTS, numeric_type()),
            Signature::new(&NUMERIC_TERNARY_ARGUMENTS, numeric_type()),
            Signature::new(&[T::Timestamp, T::Timestamp, T::Interval], T::Timestamp),
            Signature::new(&[T::TimestampTz, T::TimestampTz, T::Interval], T::TimestampTz),
            Signature::new(&[T::TimestampTz, T::TimestampTz, T::Interval, T::Text], T::TimestampTz),
        ],
}
