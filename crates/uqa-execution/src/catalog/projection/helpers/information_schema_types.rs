//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Information-schema type projection.

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::catalog::type_metadata::column_type_name;

pub fn info_datetime_precision(ty: &ColumnType) -> Value {
    if let Some(precision) = ty.temporal_precision() {
        return Value::Int(i64::from(precision));
    }
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        ColumnType::Time
        | ColumnType::TimePrecision(_)
        | ColumnType::TimeTz
        | ColumnType::TimeTzPrecision(_)
        | ColumnType::Timestamp
        | ColumnType::TimestampPrecision(_)
        | ColumnType::TimestampTz
        | ColumnType::TimestampTzPrecision(_)
        | ColumnType::Interval
        | ColumnType::IntervalWithFields { .. } => Value::Int(6),
        _ => Value::Null,
    }
}

pub fn info_interval_type(ty: &ColumnType) -> Value {
    let ColumnType::IntervalWithFields { fields, precision } = ty else {
        return Value::Null;
    };
    let fields = fields.sql_suffix().trim();
    if fields.is_empty() {
        return Value::Null;
    }
    let precision = precision.map_or_else(String::new, |precision| format!("({precision})"));
    Value::Str(format!("{}{precision}", fields.to_ascii_uppercase()))
}

pub fn info_character_maximum_length(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => {
            Value::Int(i64::from(*length))
        }
        _ => Value::Null,
    }
}

pub fn info_character_octet_length(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        // The context catalog advertises UTF8, whose maximum encoded scalar
        // width is four bytes, matching PostgreSQL's information_schema.
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => {
            Value::Int(i64::from(*length) * 4)
        }
        _ => Value::Null,
    }
}

pub fn info_numeric_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        ColumnType::SmallInteger => Value::Int(16),
        ColumnType::Integer => Value::Int(32),
        ColumnType::BigInteger => Value::Int(64),
        ColumnType::Real => Value::Int(24),
        ColumnType::DoublePrecision => Value::Int(53),
        ColumnType::Numeric {
            precision: Some(precision),
            ..
        } => Value::Int(i64::from(*precision)),
        _ => Value::Null,
    }
}

pub fn info_numeric_scale(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        ColumnType::Numeric {
            scale: Some(scale), ..
        } => Value::Int(i64::from(*scale)),
        _ => Value::Null,
    }
}

pub use uqa_sql::catalog::type_metadata::pg_type_name as info_udt_name;

pub fn info_data_type(ty: &ColumnType) -> &str {
    match ty {
        ColumnType::Array(_) => "ARRAY",
        ColumnType::Enum(_) | ColumnType::Composite(_) => "USER-DEFINED",
        _ => column_type_name(ty),
    }
}

pub fn array_dimension_count(ty: &ColumnType) -> i64 {
    uqa_sql::catalog::relation_attributes::array_dimension_count(ty)
}
