//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type markers use Core's original generic-value interpretation.

pub(crate) use uqa_core::json::JsonFieldProjection as FieldProjection;
pub(super) use uqa_core::json::{observe_json_value as observe, read_json_unsigned as read};
