//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed public ABI representations. Codes are independent of native discriminants.

use std::ffi::c_char;

pub type UQAStatus = i32;
pub const UQA_OK: UQAStatus = 0;
pub const UQA_DONE: UQAStatus = 1;
pub const UQA_TIMEOUT: UQAStatus = 2;
pub const UQA_ERROR_INVALID_ARGUMENT: UQAStatus = -1;
pub const UQA_ERROR_STATE: UQAStatus = -2;
pub const UQA_ERROR_TYPE: UQAStatus = -3;
pub const UQA_ERROR_RANGE: UQAStatus = -4;
pub const UQA_ERROR_UNSUPPORTED: UQAStatus = -5;
pub const UQA_ERROR_BUSY: UQAStatus = -6;
pub const UQA_ERROR_OUT_OF_MEMORY: UQAStatus = -7;
pub const UQA_ERROR_SQL: UQAStatus = -8;
pub const UQA_ERROR_IO: UQAStatus = -9;
pub const UQA_ERROR_PANIC: UQAStatus = -10;
pub const UQA_ERROR_LIMIT: UQAStatus = -11;
pub const UQA_ERROR_NULL: UQAStatus = -12;
pub const UQA_ERROR_NOTIFICATION: UQAStatus = -13;

pub const UQA_SUBSCRIPTION_OPTIONS_VERSION: u32 = 1;
pub const UQA_SQL_NOTIFICATION_OPTIONS_VERSION: u32 = 1;

pub const UQA_NOTIFICATION_FAILURE_INVALID_REQUEST: u32 = 1;
pub const UQA_NOTIFICATION_FAILURE_AUTHENTICATION: u32 = 2;
pub const UQA_NOTIFICATION_FAILURE_AUTHORITY_REVOKED: u32 = 3;
pub const UQA_NOTIFICATION_FAILURE_UNSUPPORTED: u32 = 4;
pub const UQA_NOTIFICATION_FAILURE_CAPACITY: u32 = 5;
pub const UQA_NOTIFICATION_FAILURE_BACKPRESSURE: u32 = 6;
pub const UQA_NOTIFICATION_FAILURE_PROTOCOL: u32 = 7;
pub const UQA_NOTIFICATION_FAILURE_SOURCE_UNAVAILABLE: u32 = 8;
pub const UQA_NOTIFICATION_FAILURE_TRANSPORT: u32 = 9;
pub const UQA_NOTIFICATION_FAILURE_TIMEOUT: u32 = 10;
pub const UQA_NOTIFICATION_FAILURE_SERVER_DRAINING: u32 = 11;
pub const UQA_NOTIFICATION_FAILURE_CANCELLED: u32 = 12;
pub const UQA_NOTIFICATION_FAILURE_SEQUENCE_EXHAUSTED: u32 = 13;

pub const UQA_COMPLETION_NONE: u32 = 0;
pub const UQA_COMPLETION_COMMITTED: u32 = 1;
pub const UQA_COMPLETION_ROLLED_BACK: u32 = 2;
pub const UQA_COMPLETION_PENDING: u32 = 3;
pub const UQA_COMPLETION_IN_TRANSACTION: u32 = 4;
pub const UQA_COMPLETION_UNKNOWN: u32 = 5;

pub const UQA_COMPRESSION_NONE: u32 = 0;
pub const UQA_COMPRESSION_ZSTD: u32 = 1;
pub const UQA_COMPRESSION_LZ4: u32 = 2;

pub const UQA_VOLATILITY_IMMUTABLE: u32 = 0;
pub const UQA_VOLATILITY_STABLE: u32 = 1;
pub const UQA_VOLATILITY_VOLATILE: u32 = 2;

pub type UQAOpenFlags = u32;
pub const UQA_OPEN_MEMORY: UQAOpenFlags = 1 << 0;
pub const UQA_OPEN_SQLITE_NATIVE: UQAOpenFlags = 1 << 1;
pub const UQA_OPEN_SQLITE_KV: UQAOpenFlags = 1 << 2;
pub const UQA_OPEN_REDB: UQAOpenFlags = 1 << 3;
pub const UQA_OPEN_SQLITE_AUTO: UQAOpenFlags = 1 << 4;

pub const UQA_TRANSACTION_IDLE: u32 = 0;
pub const UQA_TRANSACTION_ACTIVE: u32 = 1;
pub const UQA_TRANSACTION_FAILED: u32 = 2;
pub const UQA_TRANSACTION_COMPLETION_PENDING: u32 = 3;

pub const UQA_RESULT_COMMAND: u32 = 0;
pub const UQA_RESULT_ROWS: u32 = 1;
pub const UQA_RESULT_UNKNOWN: u32 = 2;

pub const UQA_NOTICE_LEVEL_DEBUG: u32 = 0;
pub const UQA_NOTICE_LEVEL_LOG: u32 = 1;
pub const UQA_NOTICE_LEVEL_INFO: u32 = 2;
pub const UQA_NOTICE_LEVEL_NOTICE: u32 = 3;
pub const UQA_NOTICE_LEVEL_WARNING: u32 = 4;

pub const UQA_TYPE_UNKNOWN: u32 = 0;
pub const UQA_TYPE_BOOL: u32 = 1;
pub const UQA_TYPE_INT16: u32 = 2;
pub const UQA_TYPE_INT32: u32 = 3;
pub const UQA_TYPE_INT64: u32 = 4;
pub const UQA_TYPE_FLOAT32: u32 = 5;
pub const UQA_TYPE_FLOAT64: u32 = 6;
pub const UQA_TYPE_TEXT: u32 = 7;
pub const UQA_TYPE_CHAR: u32 = 8;
pub const UQA_TYPE_VARCHAR: u32 = 9;
pub const UQA_TYPE_BYTEA: u32 = 10;
pub const UQA_TYPE_NUMERIC: u32 = 11;
pub const UQA_TYPE_DATE: u32 = 12;
pub const UQA_TYPE_TIME: u32 = 13;
pub const UQA_TYPE_TIMETZ: u32 = 14;
pub const UQA_TYPE_TIMESTAMP: u32 = 15;
pub const UQA_TYPE_TIMESTAMPTZ: u32 = 16;
pub const UQA_TYPE_INTERVAL: u32 = 17;
pub const UQA_TYPE_JSON: u32 = 18;
pub const UQA_TYPE_JSONB: u32 = 19;
pub const UQA_TYPE_UUID: u32 = 20;
pub const UQA_TYPE_BIT: u32 = 21;
pub const UQA_TYPE_VARBIT: u32 = 22;
pub const UQA_TYPE_ARRAY: u32 = 23;
pub const UQA_TYPE_RECORD: u32 = 24;
pub const UQA_TYPE_ENUM: u32 = 25;
pub const UQA_TYPE_DOMAIN: u32 = 26;
pub const UQA_TYPE_RANGE: u32 = 27;
pub const UQA_TYPE_MULTIRANGE: u32 = 28;
pub const UQA_TYPE_VECTOR: u32 = 29;
pub const UQA_TYPE_TENSOR: u32 = 30;
pub const UQA_TYPE_VOID: u32 = 31;
pub const UQA_TYPE_OID: u32 = 32;
pub const UQA_TYPE_XID: u32 = 33;
pub const UQA_TYPE_NAME: u32 = 34;
pub const UQA_TYPE_INTERNAL_CHAR: u32 = 35;
pub const UQA_TYPE_REFCURSOR: u32 = 36;
pub const UQA_TYPE_REGPROC: u32 = 37;
pub const UQA_TYPE_REGPROCEDURE: u32 = 38;
pub const UQA_TYPE_REGCLASS: u32 = 39;
pub const UQA_TYPE_REGNAMESPACE: u32 = 40;
pub const UQA_TYPE_REGCOLLATION: u32 = 41;
pub const UQA_TYPE_REGROLE: u32 = 42;
pub const UQA_TYPE_REGTYPE: u32 = 43;
pub const UQA_TYPE_PG_NODE_TREE: u32 = 44;
pub const UQA_TYPE_ACL_ITEM: u32 = 45;
pub const UQA_TYPE_INT2VECTOR: u32 = 46;
pub const UQA_TYPE_OIDVECTOR: u32 = 47;
pub const UQA_TYPE_ANYARRAY: u32 = 48;
pub const UQA_TYPE_COMPOSITE: u32 = 49;
pub const UQA_TYPE_XML: u32 = 50;

pub const UQA_CAP_NORI: u64 = 1 << 0;
pub const UQA_CAP_KUROMOJI: u64 = 1 << 1;
pub const UQA_CAP_ARROW_FDW: u64 = 1 << 2;
pub const UQA_CAP_DUCKDB_FDW: u64 = 1 << 3;
pub const UQA_CAP_ARROW_EXPORT: u64 = 1 << 4;
pub const UQA_CAP_PERSISTENCE: u64 = 1 << 5;
pub const UQA_CAP_INDEPENDENT_SESSIONS: u64 = 1 << 6;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UQAStringView {
    pub data: *const c_char,
    pub size: usize,
}

impl Default for UQAStringView {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            size: 0,
        }
    }
}

impl From<&str> for UQAStringView {
    fn from(value: &str) -> Self {
        Self {
            data: value.as_ptr().cast(),
            size: value.len(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UQABytesView {
    pub data: *const u8,
    pub size: usize,
}

impl Default for UQABytesView {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            size: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UQABitsView {
    pub data: *const u8,
    pub size: usize,
    pub bit_length: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UQAVectorView {
    pub data: *const f32,
    pub dimensions: u32,
}

impl Default for UQAVectorView {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            dimensions: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UQATensorView {
    pub data: *const f32,
    pub row_count: u64,
    pub dimensions: u32,
}

impl Default for UQATensorView {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            row_count: 0,
            dimensions: 0,
        }
    }
}

impl Default for UQABitsView {
    fn default() -> Self {
        Self {
            data: std::ptr::null(),
            size: 0,
            bit_length: 0,
        }
    }
}
