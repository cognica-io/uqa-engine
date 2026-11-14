//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError};
use uqa_core::{
    json::JsonToken,
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken, JsonValueDecoder, Value,
};

pub(crate) struct Values<'c> {
    pub memory: &'c MemoryBudget,
    pub original: &'c CancellationToken,
    pub invoking: &'c CancellationToken,
}

impl Values<'_> {
    pub fn control(&self) -> ProductionControl<'_> {
        ProductionControl::new(self.memory, self.original, self.invoking)
    }

    pub fn value<'a>(
        &self,
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
    ) -> Result<Produced<Value>, InputError> {
        let (text, depth) = input.encoded(token)?;
        Ok(JsonValueDecoder::new(self.memory, self.original)
            .with_invoking_cancellation(self.invoking)
            .with_depth_limit(depth - 1)
            .value(text)?
            .into())
    }
}
