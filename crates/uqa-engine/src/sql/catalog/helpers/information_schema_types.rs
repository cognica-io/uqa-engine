//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Information-schema type projection.

use crate::sql::column_type_name;
use uqa_core::Value;
use uqa_sql::ast::ColumnType;

pub(in crate::sql::catalog) fn info_datetime_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Time | ColumnType::TimeTz | ColumnType::Timestamp | ColumnType::TimestampTz => {
            Value::Int(6)
        }
        _ => Value::Null,
    }
}

pub(in crate::sql::catalog) fn info_character_maximum_length(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => {
            Value::Int(i64::from(*length))
        }
        _ => Value::Null,
    }
}

pub(in crate::sql::catalog) fn info_character_octet_length(ty: &ColumnType) -> Value {
    match ty {
        // The engine catalog advertises UTF8, whose maximum encoded scalar
        // width is four bytes, matching PostgreSQL's information_schema.
        ColumnType::Character(length) | ColumnType::Varchar(Some(length)) => {
            Value::Int(i64::from(*length) * 4)
        }
        _ => Value::Null,
    }
}

pub(in crate::sql::catalog) fn info_numeric_precision(ty: &ColumnType) -> Value {
    match ty {
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

pub(in crate::sql::catalog) fn info_numeric_scale(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Numeric {
            scale: Some(scale), ..
        } => Value::Int(i64::from(*scale)),
        _ => Value::Null,
    }
}

pub(in crate::sql::catalog) fn info_udt_name(ty: &ColumnType) -> String {
    match ty {
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
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::SmallInteger => "_int2".into(),
            ColumnType::Integer => "_int4".into(),
            ColumnType::BigInteger => "_int8".into(),
            ColumnType::Oid => "_oid".into(),
            ColumnType::Xid => "_xid".into(),
            ColumnType::Boolean => "_bool".into(),
            ColumnType::Void => "_void".into(),
            ColumnType::Text => "_text".into(),
            ColumnType::RefCursor => "_refcursor".into(),
            ColumnType::Name => "_name".into(),
            ColumnType::Uuid => "_uuid".into(),
            ColumnType::Varchar(_) => "_varchar".into(),
            ColumnType::Bpchar | ColumnType::Character(_) => "_bpchar".into(),
            ColumnType::Real => "_float4".into(),
            ColumnType::DoublePrecision => "_float8".into(),
            ColumnType::Numeric { .. } => "_numeric".into(),
            ColumnType::Json => "_json".into(),
            ColumnType::JsonB => "_jsonb".into(),
            ColumnType::Bytea => "_bytea".into(),
            ColumnType::InternalChar => "_char".into(),
            ColumnType::Regproc => "_regproc".into(),
            ColumnType::Regprocedure => "_regprocedure".into(),
            ColumnType::Regclass => "_regclass".into(),
            ColumnType::Regnamespace => "_regnamespace".into(),
            ColumnType::Regrole => "_regrole".into(),
            ColumnType::Regtype => "_regtype".into(),
            ColumnType::PgNodeTree => "_pg_node_tree".into(),
            ColumnType::AclItem => "_aclitem".into(),
            ColumnType::Int2Vector => "_int2vector".into(),
            ColumnType::OidVector => "_oidvector".into(),
            ColumnType::AnyArray => "_anyarray".into(),
            ColumnType::Record => "_record".into(),
            ColumnType::Date => "_date".into(),
            ColumnType::Time => "_time".into(),
            ColumnType::TimeTz => "_timetz".into(),
            ColumnType::Timestamp => "_timestamp".into(),
            ColumnType::TimestampTz => "_timestamptz".into(),
            ColumnType::Interval => "_interval".into(),
            ColumnType::Vector(_) => "_vector".into(),
            ColumnType::Tensor(_) => "_tensor".into(),
            ColumnType::Domain { name, .. } => format!("_{name}"),
            ColumnType::Range(subtype) => format!("_{}", subtype.range_name()),
            ColumnType::Multirange(subtype) => format!("_{}", subtype.multirange_name()),
            ColumnType::Array(_) => info_udt_name(element),
        },
        ColumnType::Date => "date".into(),
        ColumnType::Time => "time".into(),
        ColumnType::TimeTz => "timetz".into(),
        ColumnType::Timestamp => "timestamp".into(),
        ColumnType::TimestampTz => "timestamptz".into(),
        ColumnType::Interval => "interval".into(),
        ColumnType::Vector(_) => "vector".into(),
        ColumnType::Tensor(_) => "tensor".into(),
        ColumnType::Domain { name, .. } => name.clone(),
    }
}

pub(in crate::sql::catalog) fn info_data_type(ty: &ColumnType) -> &str {
    if matches!(ty, ColumnType::Array(_)) {
        "ARRAY"
    } else {
        column_type_name(ty)
    }
}

pub(in crate::sql::catalog) fn array_dimension_count(ty: &ColumnType) -> i64 {
    let mut dimensions = 0_i64;
    let mut current = ty;
    while let ColumnType::Array(element) = current {
        dimensions += 1;
        current = element;
    }
    dimensions
}
