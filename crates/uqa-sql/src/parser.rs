//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scanner settings and diagnostics at SQL compilation boundaries.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::{NoticeLevel, SQLError, SQLNotice};

/// Settings captured before parsing a complete SQL message. A later SET in that message cannot change its already parsed string values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserSettings {
    pub standard_conforming_strings: bool,
    pub backslash_quote: bool,
    pub escape_string_warning: bool,
}

impl Default for ParserSettings {
    fn default() -> Self {
        Self {
            standard_conforming_strings: true,
            backslash_quote: true,
            escape_string_warning: true,
        }
    }
}

impl ParserSettings {
    /// Read normalized session settings, falling back to `PostgreSQL`'s boot values for absent overrides.
    pub fn from_settings<'a>(setting: impl Fn(&str) -> Option<&'a str>) -> Self {
        Self {
            standard_conforming_strings: setting("standard_conforming_strings") != Some("off"),
            backslash_quote: setting("backslash_quote") != Some("off"),
            escape_string_warning: setting("escape_string_warning") != Some("off"),
        }
    }

    fn options(self, mode: pg_query::ParseMode) -> pg_query::ParseOptions {
        pg_query::ParseOptions {
            mode,
            standard_conforming_strings: self.standard_conforming_strings,
            backslash_quote: self.backslash_quote,
            escape_string_warning: self.escape_string_warning,
        }
    }
}

/// A cached statement's lexical identity and unfiltered notices. A cache hit replays notices through the current client's message filter.
#[derive(Debug, Clone, Default)]
pub struct ParserMetadata {
    pub settings: ParserSettings,
    pub notices: Arc<[SQLNotice]>,
}

#[derive(Clone)]
struct Context {
    settings: ParserSettings,
    notices: Rc<RefCell<Vec<SQLNotice>>>,
    report: bool,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

struct Scope(Option<Context>);

impl Scope {
    fn enter(context: Context) -> Self {
        Self(CONTEXT.with(|current| current.replace(Some(context))))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CONTEXT.with(|current| current.replace(self.0.take()));
    }
}

/// Compile synchronously under captured scanner settings. This scope ends before execution; nested routine compilation obtains fresh settings from its live session after applying the routine's own SET clauses.
pub fn with_settings<T>(
    settings: ParserSettings,
    compile: impl FnOnce() -> Result<T, SQLError>,
) -> (Result<T, SQLError>, ParserMetadata) {
    let notices = Rc::new(RefCell::new(Vec::new()));
    let _scope = Scope::enter(Context {
        settings,
        notices: Rc::clone(&notices),
        report: true,
    });
    let result = compile();
    let notices = std::mem::take(&mut *notices.borrow_mut()).into();
    (result, ParserMetadata { settings, notices })
}

pub(crate) fn settings() -> ParserSettings {
    CONTEXT.with(|context| {
        context
            .borrow()
            .as_ref()
            .map_or_else(ParserSettings::default, |context| context.settings)
    })
}

/// Internal lowering can revisit text which the original parser already checked. Keep lexical values without duplicating its notices.
pub(crate) fn without_notices<T>(
    compile: impl FnOnce() -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let context = CONTEXT.with(|context| context.borrow().clone());
    let _scope = context.map(|mut context| {
        context.report = false;
        Scope::enter(context)
    });
    compile()
}

fn finish<T>(outcome: pg_query::ParseOutcome<T>) -> Result<T, SQLError> {
    CONTEXT.with(|context| {
        let context = context.borrow();
        if let Some(context) = context.as_ref().filter(|context| context.report) {
            context
                .notices
                .borrow_mut()
                .extend(outcome.diagnostics.into_iter().filter_map(|diagnostic| {
                    let level = match diagnostic.severity {
                        10..=14 => NoticeLevel::Debug,
                        15 | 16 => NoticeLevel::Log,
                        17 => NoticeLevel::Info,
                        18 => NoticeLevel::Notice,
                        19 => NoticeLevel::Warning,
                        _ => return None,
                    };
                    Some(SQLNotice {
                        level,
                        sqlstate: diagnostic.sqlstate,
                        message: diagnostic.message,
                        detail: diagnostic.detail,
                        hint: diagnostic.hint,
                    })
                }));
        }
    });
    outcome.result.map_err(Into::into)
}

pub(crate) fn parse(sql: &str) -> Result<pg_query::ParseResult, SQLError> {
    parse_with_mode(sql, pg_query::ParseMode::Default)
}

pub(crate) fn parse_with_mode(
    sql: &str,
    mode: pg_query::ParseMode,
) -> Result<pg_query::ParseResult, SQLError> {
    finish(pg_query::parse_with_options(sql, settings().options(mode)))
}

pub(crate) fn parse_plpgsql(
    sql: &str,
    catalog: Option<&pg_query::PlpgsqlCatalog>,
) -> Result<serde_json::Value, SQLError> {
    finish(pg_query::parse_plpgsql_with_options(
        sql,
        catalog,
        settings().options(pg_query::ParseMode::Default),
    ))
}

/// Token metadata for source that the raw parser has already accepted; warning delivery belongs to that original parse.
pub(crate) fn scan(sql: &str) -> Result<pg_query::protobuf::ScanResult, SQLError> {
    let mut options = settings().options(pg_query::ParseMode::Default);
    options.escape_string_warning = false;
    pg_query::scan_with_options(sql, options).map_err(Into::into)
}

#[cfg(test)]
mod tests;
