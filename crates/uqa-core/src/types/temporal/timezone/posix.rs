//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use tz::{
    timezone::{Julian0WithLeap, Julian1WithoutLeap, MonthWeekDay, RuleDay},
    UtcDateTime,
};

use super::Zone;

/// POSIX rules retain their original offsets and transition times. The shared
/// `tz-rs` rule-day types validate calendar fields, but its `AlternateTime`
/// restricts offsets and rejects perpetual daylight time that `PostgreSQL` accepts.
#[derive(Clone, Copy, Debug)]
pub(super) struct PosixTimeZone {
    standard: i32,
    daylight: i32,
    start: Transition,
    end: Transition,
}

#[derive(Clone, Copy, Debug)]
struct Transition {
    day: RuleDay,
    seconds: i32,
}

pub(super) fn parse(name: &str) -> Option<Zone> {
    let mut input = Parser(name.as_bytes());
    input.designation()?;
    let standard = -input.offset()?;
    if input.0.is_empty() {
        return Some(Zone::Fixed(standard));
    }
    if input.designation()? == 0 {
        return None;
    }
    let daylight = if input.0.is_empty() || matches!(input.0.first(), Some(b',' | b';')) {
        standard + 3_600
    } else {
        -input.offset()?
    };
    let (start, end) = if input.0.is_empty() {
        (
            Transition::month_week_day(3, 2, 0)?,
            Transition::month_week_day(11, 1, 0)?,
        )
    } else {
        if !input.eat(b',') && !input.eat(b';') {
            return None;
        }
        let start = input.transition()?;
        if !input.eat(b',') {
            return None;
        }
        let end = input.transition()?;
        (start, end)
    };
    input.0.is_empty().then_some(Zone::Posix(PosixTimeZone {
        standard,
        daylight,
        start,
        end,
    }))
}

enum YearTransitions {
    Changes(i64, i64),
    NoChanges,
}

impl PosixTimeZone {
    pub(super) fn offset_at(self, unix_seconds: i64) -> Option<i32> {
        let current_year = UtcDateTime::from_timespec(unix_seconds, 0).ok()?.year();
        let mut latest = None;
        // Times up to a week outside their nominal day can cross the year boundary.
        // A full Gregorian cycle also handles rules whose transitions disappear in
        // some years. Ordinary rules finish after inspecting three adjacent years.
        for distance in 0..=401 {
            let year = current_year.checked_add(1)?.checked_sub(distance)?;
            if let YearTransitions::Changes(start, end) = self.transitions(year)? {
                for (at, offset) in [(start, self.daylight), (end, self.standard)] {
                    if at <= unix_seconds && latest.is_none_or(|(last, _)| at > last) {
                        latest = Some((at, offset));
                    }
                }
            }
            if distance >= 2 && latest.is_some() {
                break;
            }
        }
        // No transitions in a complete cycle denotes perpetual daylight time.
        Some(latest.map_or(self.daylight, |(_, offset)| offset))
    }

    pub(super) fn offset_for_local(self, local_seconds: i64) -> Option<i32> {
        let mut chosen = None;
        for offset in [self.standard, self.daylight] {
            let utc = local_seconds.checked_sub(i64::from(offset))?;
            if self.offset_at(utc)? == offset {
                chosen = Some(chosen.map_or(offset, |previous: i32| previous.min(offset)));
            }
        }
        // A fold selects the later UTC interpretation (smaller offset). In a gap
        // neither interpretation is valid, and the smaller offset is the pre-gap one.
        Some(chosen.unwrap_or_else(|| self.standard.min(self.daylight)))
    }

    fn transitions(self, year: i32) -> Option<YearTransitions> {
        let start = self
            .start
            .unix_seconds(year)?
            .checked_sub(i64::from(self.standard))?;
        let end = self
            .end
            .unix_seconds(year)?
            .checked_sub(i64::from(self.daylight))?;
        let year_seconds = if leap_year(year) { 366 } else { 365 } * 86_400;
        let valid = end < start
            || (start < end
                && end - start < year_seconds + i64::from(self.daylight - self.standard));
        Some(if valid {
            YearTransitions::Changes(start, end)
        } else {
            YearTransitions::NoChanges
        })
    }
}

impl Transition {
    fn month_week_day(month: u8, week: u8, day: u8) -> Option<Self> {
        Some(Self {
            day: RuleDay::MonthWeekDay(MonthWeekDay::new(month, week, day).ok()?),
            seconds: 7_200,
        })
    }

    fn unix_seconds(self, year: i32) -> Option<i64> {
        let (month, day) = match self.day {
            RuleDay::Julian0WithLeap(day) => (1, i64::from(day.get())),
            RuleDay::Julian1WithoutLeap(day) => (
                1,
                i64::from(day.get()) - 1 + i64::from(day.get() >= 60 && leap_year(year)),
            ),
            RuleDay::MonthWeekDay(day) => {
                let first = UtcDateTime::new(year, day.month(), 1, 0, 0, 0, 0)
                    .ok()?
                    .unix_time();
                let weekday = (first.div_euclid(86_400) + 4).rem_euclid(7);
                let mut date = (i64::from(day.week_day()) - weekday).rem_euclid(7)
                    + i64::from(day.week() - 1) * 7;
                let days = match day.month() {
                    2 => 28 + i64::from(leap_year(year)),
                    4 | 6 | 9 | 11 => 30,
                    _ => 31,
                };
                if date >= days {
                    date -= 7;
                }
                (day.month(), date)
            }
        };
        UtcDateTime::new(year, month, 1, 0, 0, 0, 0)
            .ok()?
            .unix_time()
            .checked_add(day * 86_400 + i64::from(self.seconds))
    }
}

fn leap_year(year: i32) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

struct Parser<'a>(&'a [u8]);

impl Parser<'_> {
    fn eat(&mut self, byte: u8) -> bool {
        if self.0.first() == Some(&byte) {
            self.0 = &self.0[1..];
            true
        } else {
            false
        }
    }

    fn designation(&mut self) -> Option<usize> {
        if self.eat(b'<') {
            let end = self.0.iter().position(|byte| *byte == b'>')?;
            self.0 = &self.0[end + 1..];
            Some(end)
        } else {
            let end = self
                .0
                .iter()
                .position(|byte| byte.is_ascii_digit() || matches!(byte, b',' | b'-' | b'+'))
                .unwrap_or(self.0.len());
            self.0 = &self.0[end..];
            Some(end)
        }
    }

    fn number(&mut self, minimum: u16, maximum: u16) -> Option<u16> {
        if !self.0.first()?.is_ascii_digit() {
            return None;
        }
        let mut value = 0_u16;
        while let Some(byte) = self.0.first().filter(|byte| byte.is_ascii_digit()) {
            value = value
                .checked_mul(10)?
                .checked_add(u16::from(*byte - b'0'))?;
            if value > maximum {
                return None;
            }
            self.0 = &self.0[1..];
        }
        (value >= minimum).then_some(value)
    }

    fn offset(&mut self) -> Option<i32> {
        let sign = if self.eat(b'-') {
            -1
        } else {
            self.eat(b'+');
            1
        };
        let mut seconds = i32::from(self.number(0, 167)?) * 3_600;
        if self.eat(b':') {
            seconds += i32::from(self.number(0, 59)?) * 60;
            if self.eat(b':') {
                seconds += i32::from(self.number(0, 60)?);
            }
        }
        Some(sign * seconds)
    }

    fn transition(&mut self) -> Option<Transition> {
        let day = if self.eat(b'J') || self.eat(b'j') {
            RuleDay::Julian1WithoutLeap(Julian1WithoutLeap::new(self.number(1, 365)?).ok()?)
        } else if self.eat(b'M') || self.eat(b'm') {
            let month = u8::try_from(self.number(1, 12)?).ok()?;
            if !self.eat(b'.') {
                return None;
            }
            let week = u8::try_from(self.number(1, 5)?).ok()?;
            if !self.eat(b'.') {
                return None;
            }
            let day = u8::try_from(self.number(0, 6)?).ok()?;
            RuleDay::MonthWeekDay(MonthWeekDay::new(month, week, day).ok()?)
        } else {
            RuleDay::Julian0WithLeap(Julian0WithLeap::new(self.number(0, 365)?).ok()?)
        };
        let seconds = if self.eat(b'/') {
            self.offset()?
        } else {
            7_200
        };
        Some(Transition { day, seconds })
    }
}
