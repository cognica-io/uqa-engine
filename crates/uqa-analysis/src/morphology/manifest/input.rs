//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build provenance values directly from the shared retained JSON grammar.

use super::{DictionaryResult, Value, Work};

pub(crate) async fn parse(bytes: &[u8], work: &mut impl Work) -> DictionaryResult<Value> {
    crate::json::Input::new(bytes).root(work).await
}
