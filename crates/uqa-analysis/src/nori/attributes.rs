//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Korean morphology attached to a generic analysis token.

use serde::Serialize;

use super::{NoriMorpheme, NoriOrigin, POSTag, POSType};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KoreanMorphology {
    pub pos_type: POSType,
    pub left_pos: POSTag,
    pub right_pos: POSTag,
    pub reading: Option<String>,
    pub morphemes: Option<Vec<NoriMorpheme>>,
    pub origin: NoriOrigin,
}
