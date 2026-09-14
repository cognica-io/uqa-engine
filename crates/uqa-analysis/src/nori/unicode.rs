//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated JVM classification and simple lowercase intervals.

use super::error::invalid;
use super::io::{vector, Reader};
use super::DictionaryResult;

pub(super) const CODE_POINTS: u32 = 0x11_0000;

/// Pinned Java Character values, including surrogate code points and simple lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnicodeProperties {
    pub category: u8,
    pub script: u16,
    pub is_digit: bool,
    pub is_whitespace: bool,
    pub is_space_char: bool,
    pub lowercase: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Properties {
    pub category: u8,
    pub flags: u8,
    pub script: u16,
    pub lowercase_delta: i32,
}

#[derive(Debug)]
pub(super) struct UnicodeRange {
    pub end: u32,
    pub properties: Properties,
}

#[derive(Debug)]
pub(super) struct UnicodeTable {
    pub ranges: Vec<UnicodeRange>,
}

impl UnicodeTable {
    pub fn get(&self, code_point: u32) -> Option<UnicodeProperties> {
        if code_point >= CODE_POINTS {
            return None;
        }
        let index = self.ranges.partition_point(|range| range.end <= code_point);
        let properties = self.ranges[index].properties;
        Some(UnicodeProperties {
            category: properties.category,
            script: properties.script,
            is_digit: properties.flags & 1 != 0,
            is_whitespace: properties.flags & 2 != 0,
            is_space_char: properties.flags & 4 != 0,
            lowercase: (i64::from(code_point) + i64::from(properties.lowercase_delta)) as u32,
        })
    }

    pub fn decode(reader: &mut Reader<'_>, script_count: usize) -> DictionaryResult<Self> {
        if reader.u32()? != CODE_POINTS {
            return Err(reader.invalid("incomplete Unicode profile"));
        }
        let count = reader.count(12)?;
        let mut ranges = vector(count)?;
        for _ in 0..count {
            ranges.push(UnicodeRange {
                end: reader.u32()?,
                properties: Properties {
                    category: reader.u8()?,
                    flags: reader.u8()?,
                    script: reader.u16()?,
                    lowercase_delta: reader.i32()?,
                },
            });
        }
        let table = Self { ranges };
        table.validate(script_count)?;
        Ok(table)
    }

    pub fn validate(&self, script_count: usize) -> DictionaryResult<()> {
        let mut start = 0;
        let mut previous = None;
        for range in &self.ranges {
            let p = range.properties;
            if range.end <= start
                || range.end > CODE_POINTS
                || p.category > 30
                || p.category == 17
                || p.flags > 7
                || p.script as usize >= script_count
                || previous == Some(p)
            {
                return Err(invalid(
                    "Unicode profile",
                    "invalid or noncanonical interval",
                ));
            }
            let lower_start = i64::from(start) + i64::from(p.lowercase_delta);
            let lower_end = i64::from(range.end) + i64::from(p.lowercase_delta);
            if lower_start < 0 || lower_end > i64::from(CODE_POINTS) {
                return Err(invalid(
                    "Unicode profile",
                    "lowercase value exceeds Unicode",
                ));
            }
            let surrogate = start < 0xe000 && range.end > 0xd800;
            if surrogate {
                if start < 0xd800
                    || range.end > 0xe000
                    || p.category != 19
                    || p.flags != 0
                    || p.lowercase_delta != 0
                {
                    return Err(invalid(
                        "Unicode profile",
                        "invalid surrogate classification",
                    ));
                }
            } else if p.category == 19 || (lower_start < 0xe000 && lower_end > 0xd800) {
                return Err(invalid(
                    "Unicode profile",
                    "scalar maps to an unpaired surrogate",
                ));
            }
            start = range.end;
            previous = Some(p);
        }
        if start != CODE_POINTS {
            return Err(invalid("Unicode profile", "incomplete code point coverage"));
        }
        Ok(())
    }

    #[cfg(any(test, feature = "nori-tools"))]
    pub fn encode(&self, output: &mut super::io::Writer) -> DictionaryResult<()> {
        output.u32(CODE_POINTS)?;
        output.count(self.ranges.len())?;
        for range in &self.ranges {
            output.u32(range.end)?;
            output.u8(range.properties.category)?;
            output.u8(range.properties.flags)?;
            output.u16(range.properties.script)?;
            output.i32(range.properties.lowercase_delta)?;
        }
        Ok(())
    }
}
