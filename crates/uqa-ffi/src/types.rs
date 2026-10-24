//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed native SQL descriptors; ABI kind codes never use Rust enum discriminants.

use crate::{
    abi::{
        UQAStringView, UQA_DONE, UQA_ERROR_RANGE, UQA_OK, UQA_TYPE_ACL_ITEM, UQA_TYPE_ANYARRAY,
        UQA_TYPE_ARRAY, UQA_TYPE_BIT, UQA_TYPE_BOOL, UQA_TYPE_BYTEA, UQA_TYPE_CHAR,
        UQA_TYPE_COMPOSITE, UQA_TYPE_DATE, UQA_TYPE_DOMAIN, UQA_TYPE_ENUM, UQA_TYPE_FLOAT32,
        UQA_TYPE_FLOAT64, UQA_TYPE_INT16, UQA_TYPE_INT2VECTOR, UQA_TYPE_INT32, UQA_TYPE_INT64,
        UQA_TYPE_INTERNAL_CHAR, UQA_TYPE_INTERVAL, UQA_TYPE_JSON, UQA_TYPE_JSONB,
        UQA_TYPE_MULTIRANGE, UQA_TYPE_NAME, UQA_TYPE_NUMERIC, UQA_TYPE_OID, UQA_TYPE_OIDVECTOR,
        UQA_TYPE_PG_NODE_TREE, UQA_TYPE_RANGE, UQA_TYPE_RECORD, UQA_TYPE_REFCURSOR,
        UQA_TYPE_REGCLASS, UQA_TYPE_REGCOLLATION, UQA_TYPE_REGNAMESPACE, UQA_TYPE_REGPROC,
        UQA_TYPE_REGPROCEDURE, UQA_TYPE_REGROLE, UQA_TYPE_REGTYPE, UQA_TYPE_TENSOR, UQA_TYPE_TEXT,
        UQA_TYPE_TIME, UQA_TYPE_TIMESTAMP, UQA_TYPE_TIMESTAMPTZ, UQA_TYPE_TIMETZ, UQA_TYPE_UNKNOWN,
        UQA_TYPE_UUID, UQA_TYPE_VARBIT, UQA_TYPE_VARCHAR, UQA_TYPE_VECTOR, UQA_TYPE_VOID,
        UQA_TYPE_XID,
    },
    boundary,
    error::UQAError,
};
use uqa_sql::{ast::ColumnType, catalog::type_metadata};

#[repr(transparent)]
pub struct UQAType(ColumnType);

pub(crate) fn view(ty: &ColumnType) -> *const UQAType {
    std::ptr::from_ref(ty).cast()
}

fn kind(ty: &ColumnType) -> u32 {
    match ty {
        ColumnType::Named(_) => UQA_TYPE_UNKNOWN,
        ColumnType::Boolean => UQA_TYPE_BOOL,
        ColumnType::SmallInteger => UQA_TYPE_INT16,
        ColumnType::Integer => UQA_TYPE_INT32,
        ColumnType::BigInteger => UQA_TYPE_INT64,
        ColumnType::Real => UQA_TYPE_FLOAT32,
        ColumnType::DoublePrecision => UQA_TYPE_FLOAT64,
        ColumnType::Text => UQA_TYPE_TEXT,
        ColumnType::Bpchar | ColumnType::Character(_) => UQA_TYPE_CHAR,
        ColumnType::Varchar(_) => UQA_TYPE_VARCHAR,
        ColumnType::Bytea => UQA_TYPE_BYTEA,
        ColumnType::Numeric { .. } => UQA_TYPE_NUMERIC,
        ColumnType::Date => UQA_TYPE_DATE,
        ColumnType::Time | ColumnType::TimePrecision(_) => UQA_TYPE_TIME,
        ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => UQA_TYPE_TIMETZ,
        ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => UQA_TYPE_TIMESTAMP,
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => UQA_TYPE_TIMESTAMPTZ,
        ColumnType::Interval | ColumnType::IntervalWithFields { .. } => UQA_TYPE_INTERVAL,
        ColumnType::Json => UQA_TYPE_JSON,
        ColumnType::JsonB => UQA_TYPE_JSONB,
        ColumnType::Uuid => UQA_TYPE_UUID,
        ColumnType::Bit(_) => UQA_TYPE_BIT,
        ColumnType::VarBit(_) => UQA_TYPE_VARBIT,
        ColumnType::Array(_) => UQA_TYPE_ARRAY,
        ColumnType::Record => UQA_TYPE_RECORD,
        ColumnType::Enum(_) => UQA_TYPE_ENUM,
        ColumnType::Composite(_) => UQA_TYPE_COMPOSITE,
        ColumnType::Domain { .. } => UQA_TYPE_DOMAIN,
        ColumnType::Range(_) | ColumnType::DeclaredRange(_) => UQA_TYPE_RANGE,
        ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_) => UQA_TYPE_MULTIRANGE,
        ColumnType::Vector(_) => UQA_TYPE_VECTOR,
        ColumnType::Tensor(_) => UQA_TYPE_TENSOR,
        ColumnType::Void => UQA_TYPE_VOID,
        ColumnType::Oid => UQA_TYPE_OID,
        ColumnType::Xid => UQA_TYPE_XID,
        ColumnType::Name => UQA_TYPE_NAME,
        ColumnType::InternalChar => UQA_TYPE_INTERNAL_CHAR,
        ColumnType::RefCursor => UQA_TYPE_REFCURSOR,
        ColumnType::Regproc => UQA_TYPE_REGPROC,
        ColumnType::Regprocedure => UQA_TYPE_REGPROCEDURE,
        ColumnType::Regclass => UQA_TYPE_REGCLASS,
        ColumnType::Regnamespace => UQA_TYPE_REGNAMESPACE,
        ColumnType::Regcollation => UQA_TYPE_REGCOLLATION,
        ColumnType::Regrole => UQA_TYPE_REGROLE,
        ColumnType::Regtype => UQA_TYPE_REGTYPE,
        ColumnType::PgNodeTree => UQA_TYPE_PG_NODE_TREE,
        ColumnType::AclItem => UQA_TYPE_ACL_ITEM,
        ColumnType::Int2Vector => UQA_TYPE_INT2VECTOR,
        ColumnType::OidVector => UQA_TYPE_OIDVECTOR,
        ColumnType::AnyArray => UQA_TYPE_ANYARRAY,
    }
}

ffi_fn!(uqa_type_get_kind(ty: *const UQAType, out: *mut u32) {
    let out = boundary::output(out)?;
    *out = kind(&boundary::input(ty)?.0);
    Ok(UQA_OK)
});
ffi_fn!(uqa_type_get_oid(ty: *const UQAType, out: *mut u32) {
    let out = boundary::output(out)?;
    let ty = &boundary::input(ty)?.0;
    if matches!(ty, ColumnType::Named(_)) { return Ok(UQA_DONE); }
    let oid = type_metadata::pg_type_oid(ty);
    if oid == 0 { return Ok(UQA_DONE); }
    *out = u32::try_from(oid).map_err(|_| UQAError::new(UQA_ERROR_RANGE, "native type OID is outside its ABI range"))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_type_get_modifier(ty: *const UQAType, out: *mut i32) {
    let out = boundary::output(out)?;
    *out = i32::try_from(type_metadata::pg_type_modifier(&boundary::input(ty)?.0))
        .map_err(|_| UQAError::new(UQA_ERROR_RANGE, "native type modifier is outside its ABI range"))?;
    Ok(UQA_OK)
});
ffi_fn!(uqa_type_get_name(ty: *const UQAType, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    *out = type_metadata::column_type_name(&boundary::input(ty)?.0).into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_type_get_schema(ty: *const UQAType, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    let schema = match &boundary::input(ty)?.0 {
        ColumnType::Domain { schema, .. } => schema,
        ColumnType::Enum(reference) => &reference.schema,
        ColumnType::Composite(reference) => &reference.schema,
        ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => &reference.schema,
        _ => return Ok(UQA_DONE),
    };
    *out = schema.as_str().into();
    Ok(UQA_OK)
});
ffi_fn!(uqa_type_get_base_type(ty: *const UQAType, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    match &boundary::input(ty)?.0 {
        ColumnType::Domain { base, .. } => { *out = view(base); Ok(UQA_OK) },
        _ => Ok(UQA_DONE),
    }
});
ffi_fn!(uqa_type_get_element_type(ty: *const UQAType, out: *mut *const UQAType) {
    let out = boundary::output(out)?;
    match &boundary::input(ty)?.0 {
        ColumnType::Array(element) => { *out = view(element); Ok(UQA_OK) },
        _ => Ok(UQA_DONE),
    }
});
