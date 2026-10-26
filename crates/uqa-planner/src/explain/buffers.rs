//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render measured temporary-file blocks. Timing belongs to physical I/O, independently of operator timing.

use super::properties::{Object, Property};
use std::fmt::Write as _;
use uqa_core::{io_usage::BlockUsage, memory::ProductionString};
use uqa_sql::SQLError;

pub(super) fn properties(
    plan: &mut Object<'_, '_>,
    temporary: BlockUsage,
    timing: bool,
) -> Result<(), SQLError> {
    plan.insert("Temp Read Blocks", Property::Unsigned(temporary.reads))?;
    plan.insert("Temp Written Blocks", Property::Unsigned(temporary.written))?;
    if timing {
        plan.insert(
            "Temp I/O Read Time",
            Property::Float(temporary.read_time.as_secs_f64() * 1_000.0),
        )?;
        plan.insert(
            "Temp I/O Write Time",
            Property::Float(temporary.write_time.as_secs_f64() * 1_000.0),
        )?;
    }
    Ok(())
}

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    temporary: BlockUsage,
) -> Result<(), SQLError> {
    if temporary.reads != 0 || temporary.written != 0 {
        text.push_str("\n  Buffers: temp")?;
        if temporary.reads != 0 {
            let _ = write!(text, " read={}", temporary.reads);
        }
        if temporary.written != 0 {
            let _ = write!(text, " written={}", temporary.written);
        }
    }
    if !temporary.read_time.is_zero() || !temporary.write_time.is_zero() {
        text.push_str("\n  I/O Timings: temp")?;
        if !temporary.read_time.is_zero() {
            let _ = write!(
                text,
                " read={:.3}",
                temporary.read_time.as_secs_f64() * 1_000.0
            );
        }
        if !temporary.write_time.is_zero() {
            let _ = write!(
                text,
                " write={:.3}",
                temporary.write_time.as_secs_f64() * 1_000.0
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
