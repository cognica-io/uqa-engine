//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The native catalog decoder retains admitted fields, entries and identity claims across calls.

use super::{invalid, Fields};
use crate::SQLError;
use std::{
    future::Future,
    io::Read,
    sync::atomic::{AtomicUsize, Ordering},
};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, RetainedValue},
    CancellationToken,
};

pub(super) trait CatalogEntries: Sized + Send + 'static {
    type Scratch: Send + 'static;
    const MAGIC: [u8; 8];
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch);
    fn read_entry(
        input: &mut Fields<'_>,
        value: &mut Self,
        scratch: &mut Self::Scratch,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
    fn finish(self, mutations: &MemoryBudget) -> Self;
}

struct State<T> {
    decoder: ImageDecoder<T, SQLError>,
    completed: RetainedValue<AtomicUsize>,
}

/// An unpublished native catalog input. Entry, source-byte and continuation-poll limits preserve the same retained fields and identity claims. The caller retains the same source position between calls. No source SQL or callbacks execute.
pub struct CatalogImageInput<T> {
    state: Option<State<T>>,
}

impl<T> CatalogImageInput<T> {
    pub(super) fn single(
        decoder: ImageDecoder<T, SQLError>,
        memory: &MemoryBudget,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            state: Some(State {
                decoder,
                completed: RetainedValue::with_budget(AtomicUsize::new(0), memory)?,
            }),
        })
    }

    /// Decode at most the requested number of complete entries. Use `advance_with_window` to additionally limit bytes and continuation polls inside an entry.
    pub fn advance(
        &mut self,
        reader: &mut dyn Read,
        maximum_entries: usize,
    ) -> Result<Option<T>, SQLError> {
        self.advance_with_window(
            reader,
            maximum_entries,
            ImageDecodeWindow {
                bytes: usize::MAX,
                steps: usize::MAX,
            },
        )
    }

    /// Resume original fields within independent entry, byte and poll limits. Any zero limit, decoding failure, cancellation or unwind retires all original input ownership. A completed or retired decoder cannot resume.
    pub fn advance_with_window(
        &mut self,
        reader: &mut dyn Read,
        maximum_entries: usize,
        window: ImageDecodeWindow,
    ) -> Result<Option<T>, SQLError> {
        let mut state = self
            .state
            .take()
            .ok_or_else(|| invalid("retired catalog input"))?;
        if maximum_entries == 0 || window.bytes == 0 || window.steps == 0 {
            return Err(invalid("zero catalog input limit"));
        }
        let initial = state.completed.load(Ordering::Relaxed);
        let mut bytes = window.bytes;
        for _ in 0..window.steps {
            let mut source = Counted { reader, bytes: 0 };
            let result = state
                .decoder
                .advance(&mut source, ImageDecodeWindow { bytes, steps: 1 })?;
            bytes -= source.bytes;
            if result.is_some() {
                return Ok(result);
            }
            if bytes == 0 || state.completed.load(Ordering::Relaxed) - initial >= maximum_entries {
                break;
            }
        }
        self.state = Some(state);
        Ok(None)
    }
}

struct Counted<'a> {
    reader: &'a mut dyn Read,
    bytes: usize,
}
impl Read for Counted<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = self.reader.read(output)?;
        // The Core owner rejects an invalid count before accepting the response.
        if count <= output.len() {
            self.bytes += count;
        }
        Ok(count)
    }
}

pub(super) fn registry<T: CatalogEntries>(
    memory: &MemoryBudget,
    mutations: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<CatalogImageInput<T>, SQLError> {
    cancellation.check()?;
    let completed = RetainedValue::with_budget(AtomicUsize::new(0), memory)?;
    let progress = completed.clone();
    let mutations = mutations.clone();
    let decoder = ImageDecoder::new(memory, cancellation, |mut reader| async move {
        decode::<T>(reader.input(), &mutations, Some(&progress)).await
    })?;
    Ok(CatalogImageInput {
        state: Some(State { decoder, completed }),
    })
}

pub(super) fn read_registry<T: CatalogEntries>(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<T, SQLError> {
    decode_image(reader, memory, cancellation, async |binary| {
        decode::<T>(binary, memory, None).await
    })
}

async fn decode<T: CatalogEntries>(
    binary: AsyncImageInput<'_>,
    mutations: &MemoryBudget,
    progress: Option<&AtomicUsize>,
) -> Result<T, SQLError> {
    let mut retained_children = 0;
    let mut input = Fields {
        binary,
        retained_children: &mut retained_children,
    };
    let (mut value, mut scratch) = T::create(input.binary.memory);
    let count = input.header(T::MAGIC).await?;
    for index in 0..count {
        T::read_entry(&mut input, &mut value, &mut scratch).await?;
        if index + 1 < count {
            if let Some(progress) = progress {
                progress.fetch_add(1, Ordering::Relaxed);
            }
            input.binary.checkpoint().await?;
        }
    }
    input.binary.end().await?;
    Ok(T::finish(value, mutations))
}
