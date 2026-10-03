//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The levels of notices.

/// The level of a notice, as `PostgreSQL` names its message levels below `ERROR`, in increasing order of severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NoticeLevel {
    /// `DEBUG`, which `PL/pgSQL`'s `RAISE DEBUG` reports as `DEBUG1`.
    Debug,
    Log,
    Info,
    Notice,
    Warning,
}

impl NoticeLevel {
    /// The level's name, as clients show it.
    pub const fn as_str(self) -> &'static str {
        match self {
            NoticeLevel::Debug => "DEBUG",
            NoticeLevel::Log => "LOG",
            NoticeLevel::Info => "INFO",
            NoticeLevel::Notice => "NOTICE",
            NoticeLevel::Warning => "WARNING",
        }
    }

    /// The SQLSTATE of a notice whose report names none, as `errstart` assigns it: `01000` (`warning`) for a warning and `00000` (`successful_completion`) for every lower level.
    pub const fn default_sqlstate(self) -> &'static str {
        match self {
            NoticeLevel::Warning => "01000",
            NoticeLevel::Debug | NoticeLevel::Log | NoticeLevel::Info | NoticeLevel::Notice => {
                "00000"
            }
        }
    }
}
