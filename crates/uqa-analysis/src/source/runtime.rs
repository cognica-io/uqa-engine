//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable source buffers share their reservation with every retained view.

use std::sync::Arc;
use uqa_core::memory::Budgeted;
#[cfg(feature = "nori")]
use uqa_core::memory::{BudgetedString, MemoryBudget};

#[cfg(feature = "nori")]
use crate::AnalysisResult;

#[derive(Debug, Clone)]
pub(super) enum SourceText<'a> {
    Borrowed(&'a str),
    Owned(Arc<Budgeted<String>>),
}

impl SourceText<'_> {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(text) => text,
            Self::Owned(text) => text,
        }
    }

    pub fn into_string(self) -> String {
        match self {
            Self::Borrowed(text) => text.to_owned(),
            Self::Owned(text) => match Arc::try_unwrap(text) {
                Ok(text) => text.into_parts().0,
                Err(text) => text.as_str().to_owned(),
            },
        }
    }
}

#[cfg(feature = "nori")]
pub(super) fn copy_text(
    input: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Arc<Budgeted<String>>> {
    poll()?;
    let mut output = BudgetedString::new(budget);
    output.reserve(input.len())?;
    for (index, character) in input.chars().enumerate() {
        if index % 1024 == 0 {
            poll()?;
        }
        output.push(character)?;
    }
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(output, memory).into_shared()?)
}
