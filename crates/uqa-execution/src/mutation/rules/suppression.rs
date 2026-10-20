//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Union of rule suppression decisions in original event coordinates.

use parking_lot::Mutex;
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

use crate::physical::physical_exec_error;
use crate::spill::MatchFlags;

pub(crate) struct RuleSuppression {
    count: usize,
    flags: SuppressionFlags,
    cancellation: CancellationToken,
}

enum SuppressionFlags {
    None,
    All,
    Mixed(Mutex<MatchFlags>),
}

impl RuleSuppression {
    pub(crate) fn new(count: usize, cancellation: CancellationToken) -> Self {
        Self {
            count,
            flags: SuppressionFlags::None,
            cancellation,
        }
    }

    pub(crate) fn suppress(&mut self, index: usize) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if index >= self.count {
            return Err(SQLError::Internal(
                "rewrite-rule suppression ordinal is outside the event set".into(),
            ));
        }
        if matches!(self.flags, SuppressionFlags::None) {
            let flags = MatchFlags::new(self.count as u64, "rewrite-rule suppression")
                .map_err(physical_exec_error)?;
            self.flags = SuppressionFlags::Mixed(Mutex::new(flags));
        }
        if let SuppressionFlags::Mixed(flags) = &mut self.flags {
            flags
                .get_mut()
                .mark(index as u64)
                .map_err(physical_exec_error)?;
        }
        Ok(())
    }

    pub(crate) fn suppress_all(&mut self) -> Result<(), SQLError> {
        self.cancellation.check()?;
        self.flags = SuppressionFlags::All;
        Ok(())
    }

    pub(crate) fn suppresses(&self, index: usize) -> Result<bool, SQLError> {
        self.cancellation.check()?;
        if index >= self.count {
            return Ok(false);
        }
        match &self.flags {
            SuppressionFlags::None => Ok(false),
            SuppressionFlags::All => Ok(true),
            SuppressionFlags::Mixed(flags) => flags
                .lock()
                .is_marked(index as u64)
                .map_err(physical_exec_error),
        }
    }
}

#[cfg(test)]
mod tests;
