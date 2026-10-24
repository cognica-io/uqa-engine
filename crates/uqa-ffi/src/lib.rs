//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedded C ABI. Public pointer contracts are documented in `include/uqa`.

#[macro_use]
mod boundary;
pub mod abi;
mod config;
mod cursor;
mod error;
mod execute;
mod library;
mod notice;
mod result;
mod session;
mod types;
mod value;

pub use abi::*;
pub use config::*;
pub use cursor::*;
pub use error::{
    uqa_error_get_completion, uqa_error_get_context, uqa_error_get_detail, uqa_error_get_hint,
    uqa_error_get_last, uqa_error_get_message, uqa_error_get_position, uqa_error_get_sqlstate,
    uqa_error_get_statement_index, uqa_error_get_status, UQAError,
};
pub use execute::*;
pub use library::*;
pub use notice::*;
pub use result::*;
pub use session::*;
pub use types::*;
pub use value::*;
