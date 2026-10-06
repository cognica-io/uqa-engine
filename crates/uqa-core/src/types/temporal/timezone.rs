//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocation-free time zone lookup and local/UTC offset selection.

use tz::{datetime::FoundDateTimeKind, DateTime, TimeZoneRef, UtcDateTime};

mod abbreviations;
mod posix;

#[cfg(test)]
mod tests;

/// An immutable time zone backed by bundled IANA data, a fixed offset, or a POSIX rule.
///
/// Offsets are seconds east of UTC. Names and rule parsing never read host files,
/// environment variables, or the wall clock, and do not allocate.
#[derive(Clone, Copy, Debug)]
pub struct TemporalTimeZone(Zone);

#[derive(Clone, Copy, Debug)]
enum Zone {
    Named(&'static TimeZoneRef<'static>),
    Fixed(i32),
    Posix(posix::PosixTimeZone),
}

impl TemporalTimeZone {
    /// Resolve a bundled IANA name or a `PostgreSQL` POSIX time zone specification.
    /// This does not search the SQL timezone-abbreviation table.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        if name.len() > 255 || name.as_bytes().contains(&0) {
            return None;
        }
        let key = name.strip_prefix(':').unwrap_or(name);
        // The data crate's generated case-folding also equates punctuation with
        // control bytes. Admit only the alphabet used by IANA names before lookup.
        if key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'+'))
        {
            if let Some(zone) = tzdb_data::find_tz(key.as_bytes()) {
                return Some(Self(Zone::Named(zone)));
            }
        }
        if name.starts_with(':') {
            return None;
        }
        posix::parse(name).map(Self)
    }

    /// Resolve a SQL timezone argument, searching `PostgreSQL`'s Default abbreviations first.
    #[must_use]
    pub fn named_or_abbreviation(name: &str) -> Option<Self> {
        abbreviations::lookup(name).or_else(|| Self::named(name))
    }

    /// Return the case-correct spelling of a bundled name, preserving alias identity.
    #[must_use]
    pub fn canonical_name(name: &str) -> Option<&'static str> {
        let name = name.strip_prefix(':').unwrap_or(name);
        tzdb_data::TZ_NAMES
            .iter()
            .copied()
            .find(|candidate| candidate.eq_ignore_ascii_case(name))
    }

    /// Construct a fixed offset in seconds east of UTC, within the POSIX parser's range.
    #[must_use]
    pub fn fixed(offset_seconds_east: i32) -> Option<Self> {
        (-604_800..=604_800)
            .contains(&offset_seconds_east)
            .then_some(Self(Zone::Fixed(offset_seconds_east)))
    }

    /// Look up the UTC offset at an absolute Unix timestamp, in seconds.
    #[must_use]
    pub fn offset_at(self, unix_seconds: i64) -> Option<i32> {
        match self.0 {
            Zone::Fixed(offset) => Some(offset),
            Zone::Named(zone) => zone
                .find_local_time_type(unix_seconds)
                .ok()
                .map(tz::timezone::LocalTimeType::ut_offset),
            Zone::Posix(zone) => zone.offset_at(unix_seconds),
        }
    }

    /// Select the offset for civil time represented as Unix seconds without an offset.
    ///
    /// A forward gap uses the offset before the transition; a backward fold uses
    /// the offset after it. This is independent of daylight-saving labels.
    #[must_use]
    pub fn offset_for_local(self, local_unix_seconds: i64) -> Option<i32> {
        match self.0 {
            Zone::Fixed(offset) => Some(offset),
            Zone::Named(zone) => named_local_offset(*zone, local_unix_seconds),
            Zone::Posix(zone) => zone.offset_for_local(local_unix_seconds),
        }
    }
}

fn named_local_offset(zone: TimeZoneRef<'_>, local_seconds: i64) -> Option<i32> {
    let local = UtcDateTime::from_timespec(local_seconds, 0).ok()?;
    let mut candidates = [None; 2];
    let found = DateTime::find_n(
        &mut candidates,
        local.year(),
        local.month(),
        local.month_day(),
        local.hour(),
        local.minute(),
        local.second(),
        0,
        zone,
    )
    .ok()?;
    if !found.is_exhaustive() {
        return None;
    }
    found
        .data()
        .iter()
        .flatten()
        .filter_map(|candidate| {
            let offset = match candidate {
                FoundDateTimeKind::Normal(value) => value.local_time_type().ut_offset(),
                FoundDateTimeKind::Skipped {
                    before_transition, ..
                } => before_transition.local_time_type().ut_offset(),
            };
            Some((local_seconds.checked_sub(i64::from(offset))?, offset))
        })
        .max_by_key(|(utc, _)| *utc)
        .map(|(_, offset)| offset)
}
