//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Messages below `ERROR` that a statement reports without failing, with the fields of `PostgreSQL`'s `NoticeResponse`.

/// The severity of a notice, as `elog.h` levels below `ERROR` name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NoticeSeverity {
    Debug,
    Log,
    Info,
    Notice,
    Warning,
}

impl NoticeSeverity {
    /// The non-localized severity name `PostgreSQL` sends in the `V` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Log => "LOG",
            Self::Info => "INFO",
            Self::Notice => "NOTICE",
            Self::Warning => "WARNING",
        }
    }

    /// The severity a level name spells; `DEBUG1` through `DEBUG5` are `DEBUG`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.to_ascii_uppercase();
        match name.as_str() {
            "LOG" => Some(Self::Log),
            "INFO" => Some(Self::Info),
            "NOTICE" => Some(Self::Notice),
            "WARNING" => Some(Self::Warning),
            _ if name.starts_with("DEBUG") => Some(Self::Debug),
            _ => None,
        }
    }

    /// `errstart`'s default SQLSTATE: `01000` for warnings and `00000` below them.
    #[must_use]
    pub const fn default_sqlstate(self) -> &'static str {
        match self {
            Self::Warning => "01000",
            Self::Debug | Self::Log | Self::Info | Self::Notice => "00000",
        }
    }
}

/// One notice: its severity, SQLSTATE, message and optional DETAIL and HINT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLNotice {
    pub severity: NoticeSeverity,
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
}

impl SQLNotice {
    /// A notice with the severity's default SQLSTATE and no DETAIL or HINT.
    #[must_use]
    pub fn new(severity: NoticeSeverity, message: impl Into<String>) -> Self {
        Self {
            severity,
            sqlstate: severity.default_sqlstate().to_string(),
            message: message.into(),
            detail: None,
            hint: None,
        }
    }

    #[must_use]
    pub fn notice(message: impl Into<String>) -> Self {
        Self::new(NoticeSeverity::Notice, message)
    }

    #[must_use]
    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(NoticeSeverity::Warning, message)
    }

    #[must_use]
    pub fn with_sqlstate(mut self, sqlstate: impl Into<String>) -> Self {
        self.sqlstate = sqlstate.into();
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    #[must_use]
    pub fn with_hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }
}
