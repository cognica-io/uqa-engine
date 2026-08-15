//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact typed binary batch codec.

mod decode;
mod encode;

pub(crate) use decode::{decode_batch, decode_physical_row_record};
pub(crate) use encode::{
    append_batches, encode_physical_row_record, encoded_batch_overhead_size, encoded_batch_size,
    encoded_physical_row_record_size,
};

pub(super) const MAX_VALUE_DEPTH: usize = 128;
