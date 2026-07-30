//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Errors raised when scoring inputs or learned parameters violate their contract.

/// A scoring request could not be evaluated without producing an invalid score.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScoringError {
    #[error("invalid scoring input: {0}")]
    InvalidInput(String),

    #[error("scoring arithmetic overflow: {0}")]
    ArithmeticOverflow(String),
}

pub type ScoringResult<T> = Result<T, ScoringError>;

pub(crate) fn invalid_input(message: impl Into<String>) -> ScoringError {
    ScoringError::InvalidInput(message.into())
}

pub(crate) fn require_finite(value: f64, name: &str) -> ScoringResult<()> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(invalid_input(format!("{name} must be finite, got {value}")))
    }
}

pub(crate) fn require_probability(value: f64, name: &str) -> ScoringResult<()> {
    require_finite(value, name)?;
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "{name} must be in [0, 1], got {value}"
        )))
    }
}
