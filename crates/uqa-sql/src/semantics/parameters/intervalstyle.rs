//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Translate a validated `IntervalStyle` setting to Core's input/output convention.

use uqa_core::TemporalIntervalStyle;

#[must_use]
pub fn style(setting: &str) -> Option<TemporalIntervalStyle> {
    [
        ("postgres", TemporalIntervalStyle::Postgres),
        ("postgres_verbose", TemporalIntervalStyle::PostgresVerbose),
        ("sql_standard", TemporalIntervalStyle::SqlStandard),
        ("iso_8601", TemporalIntervalStyle::Iso8601),
    ]
    .into_iter()
    .find_map(|(name, style)| setting.eq_ignore_ascii_case(name).then_some(style))
}
