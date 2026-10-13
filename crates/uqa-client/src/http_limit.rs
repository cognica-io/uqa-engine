//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::HttpLimitKind;

/// Validated numeric evidence; it never authorizes retry or automatic batch splitting.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpLimit {
    pub kind: HttpLimitKind,
    pub limit: u64,
    pub actual: u64,
    pub actual_is_lower_bound: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executing_limit: Option<u64>,
}

impl HttpLimit {
    pub(crate) fn matches_code(&self, code: &str) -> bool {
        const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
        self.kind.code() == code
            && self.actual > self.limit
            && (self.kind == HttpLimitKind::Parameters || self.limit > 0)
            && self.actual <= MAX_SAFE_INTEGER
            && self.limit <= MAX_SAFE_INTEGER
            && if self.kind.is_admission() {
                self.actual_is_lower_bound
                    && self
                        .executing_limit
                        .is_some_and(|value| value > 0 && value <= MAX_SAFE_INTEGER)
            } else {
                self.executing_limit.is_none()
                    && self.actual_is_lower_bound == (self.kind == HttpLimitKind::RequestBodyBytes)
            }
    }
}

impl fmt::Display for HttpLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} limit {}; observed {}{}",
            self.kind.as_str(),
            self.limit,
            if self.actual_is_lower_bound {
                "at least "
            } else {
                ""
            },
            self.actual
        )?;
        if let Some(executing) = self.executing_limit {
            write!(formatter, "; executing limit {executing}")?;
        }
        Ok(())
    }
}
