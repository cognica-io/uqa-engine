//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog input fixtures reject physical rereads and retain independent work/output allowances.

use std::future::Future;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageRead},
    memory::MemoryBudget,
    CancellationToken,
};
use uqa_sql::SQLError;

pub(crate) struct Scope {
    pub native: MemoryBudget,
    pub work: MemoryBudget,
    pub original: CancellationToken,
    pub invoking: CancellationToken,
}
impl Default for Scope {
    fn default() -> Self {
        Self {
            native: MemoryBudget::new(4 << 20),
            work: MemoryBudget::new(4 << 20),
            original: CancellationToken::new(),
            invoking: CancellationToken::new(),
        }
    }
}
impl Scope {
    pub fn decoder<T, F>(
        &self,
        create: impl FnOnce(MemoryBudget, CancellationToken, ImageRead) -> F,
    ) -> ImageDecoder<T, SQLError>
    where
        F: Future<Output = Result<T, SQLError>> + Send + 'static,
    {
        let native = self.native.clone();
        let original = self.original.clone();
        ImageDecoder::new(&self.work, &self.invoking, |reader| {
            create(native, original, reader)
        })
        .unwrap()
    }
    pub fn assert_released(&self) {
        assert_eq!((self.native.used(), self.work.used()), (0, 0));
    }
}
struct NoReads;
impl std::io::Read for NoReads {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("native catalog selection must not reread a physical source")
    }
}
pub(crate) fn advance<T>(
    input: &mut ImageDecoder<T, SQLError>,
    steps: usize,
) -> Result<Option<T>, SQLError> {
    input.advance(&mut NoReads, ImageDecodeWindow { bytes: 1, steps })
}
pub(crate) fn complete<T>(
    input: &mut ImageDecoder<T, SQLError>,
    steps: usize,
) -> Result<(T, usize), SQLError> {
    for polls in 1..1_000_000 {
        if let Some(value) = advance(input, steps)? {
            return Ok((value, polls));
        }
    }
    panic!("native catalog input stopped progressing")
}
