//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact typed binary batch codec.

mod decode;
mod encode;

pub(crate) use decode::decode_batch;
pub(crate) use encode::{
    append_batches, encoded_batch_size, encoded_named_single_row_batch_size,
    encoded_single_row_batch_size,
};

pub(super) const MAX_VALUE_DEPTH: usize = 128;
