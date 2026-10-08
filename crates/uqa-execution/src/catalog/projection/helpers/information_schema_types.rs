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

pub fn info_udt_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        ColumnType::SmallInteger => "int2".into(),
        ColumnType::Integer => "int4".into(),
        ColumnType::BigInteger => "int8".into(),
        ColumnType::Oid => "oid".into(),
        ColumnType::Xid => "xid".into(),
        ColumnType::Boolean => "bool".into(),
        ColumnType::Void => "void".into(),
        ColumnType::Text => "text".into(),
        ColumnType::RefCursor => "refcursor".into(),
        ColumnType::Name => "name".into(),
        ColumnType::Uuid => "uuid".into(),
        ColumnType::Varchar(_) => "varchar".into(),
        ColumnType::Bpchar | ColumnType::Character(_) => "bpchar".into(),
        ColumnType::Real => "float4".into(),
        ColumnType::DoublePrecision => "float8".into(),
        ColumnType::Numeric { .. } => "numeric".into(),
        ColumnType::Json => "json".into(),
        ColumnType::JsonB => "jsonb".into(),
        ColumnType::Bytea => "bytea".into(),
        ColumnType::InternalChar => "char".into(),
        ColumnType::Regproc => "regproc".into(),
        ColumnType::Regprocedure => "regprocedure".into(),
        ColumnType::Regclass => "regclass".into(),
        ColumnType::Regcollation => "regcollation".into(),
        ColumnType::Regnamespace => "regnamespace".into(),
        ColumnType::Regrole => "regrole".into(),
        ColumnType::Regtype => "regtype".into(),
        ColumnType::PgNodeTree => "pg_node_tree".into(),
        ColumnType::AclItem => "aclitem".into(),
        ColumnType::Int2Vector => "int2vector".into(),
        ColumnType::OidVector => "oidvector".into(),
        ColumnType::AnyArray => "anyarray".into(),
        ColumnType::Record => "record".into(),
        ColumnType::Range(subtype) => subtype.range_name().into(),
        ColumnType::Multirange(subtype) => subtype.multirange_name().into(),
        // An array type is named after its element type, and a nested array shares its innermost array type.
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::Array(_) => info_udt_name(element),
            element => format!("_{}", info_udt_name(element)),
        },
        ColumnType::Date => "date".into(),
        ColumnType::Time | ColumnType::TimePrecision(_) => "time".into(),
        ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => "timetz".into(),
        ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => "timestamp".into(),
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => "timestamptz".into(),
        ColumnType::Interval | ColumnType::IntervalWithFields { .. } => "interval".into(),
        ColumnType::Vector(_) => "vector".into(),
        ColumnType::Tensor(_) => "tensor".into(),
        ColumnType::Domain { name, .. } => name.clone(),
        ColumnType::Enum(reference) => reference.name.clone(),
        ColumnType::Composite(reference) => reference.name.clone(),
    }
}

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
