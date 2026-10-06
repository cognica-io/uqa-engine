//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric, text, temporal, and array inspection signatures used by ordinary SQL.

use super::registry::{
    declarations, numeric_type, Signature, NUMERIC_BINARY_ARGUMENTS, NUMERIC_SCALE_ARGUMENTS,
    NUMERIC_TERNARY_ARGUMENTS, NUMERIC_UNARY_ARGUMENTS,
};
use crate::ast::ColumnType as T;

declarations! { pub(super) fn lookup(name);
        "array_dims" => &[Signature::new(&[T::AnyArray], T::Text)],
        "array_ndims" | "cardinality" => &[Signature::new(&[T::AnyArray], T::Integer)],
        "array_lower" | "array_upper" | "array_length" => &[
            Signature::new(&[T::AnyArray, T::Integer], T::Integer),
        ],
        "lower" | "upper" => &[Signature::new(&[T::Text], T::Text)],
        "replace" => &[Signature::new(&[T::Text, T::Text, T::Text], T::Text)],
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
        "extract" => &[
            Signature::new(&[T::Text, T::Date], numeric_type()),
            Signature::new(&[T::Text, T::Time], numeric_type()),
            Signature::new(&[T::Text, T::TimeTz], numeric_type()),
            Signature::new(&[T::Text, T::Timestamp], numeric_type()),
            Signature::new(&[T::Text, T::TimestampTz], numeric_type()),
            Signature::new(&[T::Text, T::Interval], numeric_type()),
        ],
        "date_part" => &[
            Signature::new(&[T::Text, T::Date], T::DoublePrecision),
            Signature::new(&[T::Text, T::Time], T::DoublePrecision),
            Signature::new(&[T::Text, T::TimeTz], T::DoublePrecision),
            Signature::new(&[T::Text, T::Timestamp], T::DoublePrecision),
            Signature::new(&[T::Text, T::TimestampTz], T::DoublePrecision),
            Signature::new(&[T::Text, T::Interval], T::DoublePrecision),
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
