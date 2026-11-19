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
        "like" => &[
            Signature::new(const { &[T::Text, T::Text] }, T::Boolean),
            Signature::new(const { &[T::Name, T::Text] }, T::Boolean),
            Signature::new(const { &[T::Bytea, T::Bytea] }, T::Boolean),
        ],
        "like_escape" => &[
            Signature::new(const { &[T::Text, T::Text] }, T::Text),
            Signature::new(const { &[T::Bytea, T::Bytea] }, T::Bytea),
        ],
        "similar_to_escape" => &[
            Signature::new(const { &[T::Text] }, T::Text),
            Signature::new(const { &[T::Text, T::Text] }, T::Text),
        ],
        "current_schema" => &[Signature::new(&[], T::Name)],
        "array_dims" => &[Signature::new(const { &[T::AnyArray] }, T::Text)],
        "array_ndims" | "cardinality" => &[Signature::new(const { &[T::AnyArray] }, T::Integer)],
        "array_lower" | "array_upper" | "array_length" => &[
            Signature::new(const { &[T::AnyArray, T::Integer] }, T::Integer),
        ],
        "lower" | "upper" | "initcap" => &[Signature::new(const { &[T::Text] }, T::Text)],
        "replace" => &[Signature::new(const { &[T::Text, T::Text, T::Text] }, T::Text)],
        "factorial" => &[Signature::new(const { &[T::BigInteger] }, numeric_type())],
        "mod" => &[
            Signature::new(const { &[T::SmallInteger, T::SmallInteger] }, T::SmallInteger),
            Signature::new(const { &[T::Integer, T::Integer] }, T::Integer),
            Signature::new(const { &[T::BigInteger, T::BigInteger] }, T::BigInteger),
            Signature::new(&NUMERIC_BINARY_ARGUMENTS, numeric_type()),
        ],
        "power" | "pow" => &[
            Signature::new(const { &[T::DoublePrecision, T::DoublePrecision] }, T::DoublePrecision),
            Signature::new(&NUMERIC_BINARY_ARGUMENTS, numeric_type()),
        ],
        "sqrt" => &[
            Signature::new(const { &[T::DoublePrecision] }, T::DoublePrecision),
            Signature::new(&NUMERIC_UNARY_ARGUMENTS, numeric_type()),
        ],
        "cbrt" => &[Signature::new(const { &[T::DoublePrecision] }, T::DoublePrecision)],
        "round" | "trunc" => &[
            Signature::new(const { &[T::DoublePrecision] }, T::DoublePrecision),
            Signature::new(&NUMERIC_UNARY_ARGUMENTS, numeric_type()),
            Signature::new(&NUMERIC_SCALE_ARGUMENTS, numeric_type()),
        ],
        "substr" => &[
            Signature::new(const { &[T::Text, T::Integer] }, T::Text),
            Signature::new(const { &[T::Text, T::Integer, T::Integer] }, T::Text),
            Signature::new(const { &[T::Bytea, T::Integer] }, T::Bytea),
            Signature::new(const { &[T::Bytea, T::Integer, T::Integer] }, T::Bytea),
        ],
        "substring" => &[
            Signature::new(const { &[T::Text, T::Integer] }, T::Text),
            Signature::new(const { &[T::Text, T::Integer, T::Integer] }, T::Text),
            Signature::new(const { &[T::Bytea, T::Integer] }, T::Bytea),
            Signature::new(const { &[T::Bytea, T::Integer, T::Integer] }, T::Bytea),
            Signature::new(const { &[T::Text, T::Text] }, T::Text),
            Signature::new(const { &[T::Text, T::Text, T::Text] }, T::Text),
        ],
        "extract" => &[
            Signature::new(const { &[T::Text, T::Date] }, numeric_type()),
            Signature::new(const { &[T::Text, T::Time] }, numeric_type()),
            Signature::new(const { &[T::Text, T::TimeTz] }, numeric_type()),
            Signature::new(const { &[T::Text, T::Timestamp] }, numeric_type()),
            Signature::new(const { &[T::Text, T::TimestampTz] }, numeric_type()),
            Signature::new(const { &[T::Text, T::Interval] }, numeric_type()),
        ],
        "date_part" => &[
            Signature::new(const { &[T::Text, T::Date] }, T::DoublePrecision),
            Signature::new(const { &[T::Text, T::Time] }, T::DoublePrecision),
            Signature::new(const { &[T::Text, T::TimeTz] }, T::DoublePrecision),
            Signature::new(const { &[T::Text, T::Timestamp] }, T::DoublePrecision),
            Signature::new(const { &[T::Text, T::TimestampTz] }, T::DoublePrecision),
            Signature::new(const { &[T::Text, T::Interval] }, T::DoublePrecision),
        ],
        "date_trunc" => &[
            Signature::new(const { &[T::Text, T::Timestamp] }, T::Timestamp),
            Signature::new(const { &[T::Text, T::TimestampTz] }, T::TimestampTz),
            Signature::new(const { &[T::Text, T::TimestampTz, T::Text] }, T::TimestampTz),
            Signature::new(const { &[T::Text, T::Interval] }, T::Interval),
        ],
        "timezone" => &[
            Signature::new(const { &[T::Text, T::Timestamp] }, T::TimestampTz),
            Signature::new(const { &[T::Text, T::TimestampTz] }, T::Timestamp),
            Signature::new(const { &[T::Interval, T::Timestamp] }, T::TimestampTz),
            Signature::new(const { &[T::Interval, T::TimestampTz] }, T::Timestamp),
            Signature::new(const { &[T::Timestamp] }, T::TimestampTz),
            Signature::new(const { &[T::TimestampTz] }, T::Timestamp),
        ],
        "generate_series" => &[
            Signature::new(const { &[T::Integer, T::Integer] }, T::Integer),
            Signature::new(const { &[T::Integer, T::Integer, T::Integer] }, T::Integer),
            Signature::new(const { &[T::BigInteger, T::BigInteger] }, T::BigInteger),
            Signature::new(const { &[T::BigInteger, T::BigInteger, T::BigInteger] }, T::BigInteger),
            Signature::new(&NUMERIC_BINARY_ARGUMENTS, numeric_type()),
            Signature::new(&NUMERIC_TERNARY_ARGUMENTS, numeric_type()),
            Signature::new(const { &[T::Timestamp, T::Timestamp, T::Interval] }, T::Timestamp),
            Signature::new(const { &[T::TimestampTz, T::TimestampTz, T::Interval] }, T::TimestampTz),
            Signature::new(const { &[T::TimestampTz, T::TimestampTz, T::Interval, T::Text] }, T::TimestampTz),
        ],
}
