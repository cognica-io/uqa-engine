//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use uqa_core::Value;

/// One decoded frame from the stable UQA NDJSON SQL stream.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SQLStreamFrame {
    Metadata {
        columns: Vec<String>,
        row_count: usize,
        spilled_to_disk: bool,
        request_id: String,
    },
    Row {
        row: BTreeMap<String, Value>,
    },
    Complete {
        row_count: usize,
        request_id: String,
    },
    Error {
        code: String,
        message: String,
        request_id: String,
    },
}

impl SQLStreamFrame {
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Metadata { request_id, .. }
            | Self::Complete { request_id, .. }
            | Self::Error { request_id, .. } => Some(request_id),
            Self::Row { .. } => None,
        }
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Error { .. })
    }
}

impl fmt::Debug for SQLStreamFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SQLStreamFrame([REDACTED])")
    }
}
