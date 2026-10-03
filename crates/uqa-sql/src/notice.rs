//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Notices that a statement reports without failing.

pub mod level;

pub use level::NoticeLevel;

/// A notice that a statement reports without failing, as `PostgreSQL`'s `ereport` sends one below `ERROR`: its level, SQLSTATE and primary message, and the detail and hint that clients receive as fields of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLNotice {
    pub level: NoticeLevel,
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
}

impl SQLNotice {
    /// A notice of `level` that carries the SQLSTATE `PostgreSQL` assigns a report that names none (see [`NoticeLevel::default_sqlstate`]).
    pub fn new(level: NoticeLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            sqlstate: level.default_sqlstate().to_string(),
            message: message.into(),
            detail: None,
            hint: None,
        }
    }

    /// A `NOTICE` with SQLSTATE `00000`.
    pub fn notice(message: impl Into<String>) -> Self {
        Self::new(NoticeLevel::Notice, message)
    }

    /// A `WARNING` with SQLSTATE `01000`.
    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(NoticeLevel::Warning, message)
    }

    /// The notice with the SQLSTATE its report names.
    #[must_use]
    pub fn with_sqlstate(mut self, sqlstate: impl Into<String>) -> Self {
        self.sqlstate = sqlstate.into();
        self
    }

    /// The notice with a detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// The notice with a hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}
