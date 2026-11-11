//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original configuration images reconstruct native stages without resolving resources.

mod input;

use crate::Analyzer;
use input::{Input, Result};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput},
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

pub(super) fn decode(
    bytes: &[u8],
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Analyzer>> {
    struct NoRead;
    impl std::io::Read for NoRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            unreachable!("borrowed JSON does not read the image source")
        }
    }
    decode_image(&mut NoRead, memory, cancellation, async |input| {
        decode_from(bytes, input).await
    })
}

pub(super) async fn decode_from(
    bytes: &[u8],
    work: AsyncImageInput<'_>,
) -> Result<Budgeted<Analyzer>> {
    let mut input = Input::new(bytes, work)?;
    let config = crate::configuration::analyzer(&mut input).await?;
    input.finish(config).await
}
