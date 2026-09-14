//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Modified Hepburn reading rules use at most three raw units of lookahead.

mod tables;

use crate::morphology::filter::Work;
use crate::AnalysisResult;

pub(super) fn visit(
    mut input: impl Iterator<Item = u16>,
    work: &mut Work<'_>,
    mut emit: impl FnMut(u16) -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    let mut pending = [input.next(), input.next(), input.next()];
    while let Some(first) = pending[0] {
        work.tick()?;
        let (replacement, consumed) = rule(first, pending[1].unwrap_or(0), pending[2].unwrap_or(0));
        if let Some(replacement) = replacement {
            for unit in replacement.encode_utf16() {
                emit(unit)?;
            }
        } else {
            emit(first)?;
        }
        for _ in 0..consumed {
            pending = [pending[1], pending[2], input.next()];
        }
    }
    Ok(())
}

fn rule(first: u16, second: u16, third: u16) -> (Option<&'static str>, usize) {
    match first {
        0x30c3 => return (Some(geminate(second)), 1),
        0x30f3 => return (Some(nasal(second)), 1),
        0x30fc => return (Some(""), 1),
        _ => {}
    }
    if let Some(value) = lookup(tables::TRIPLES, [first, second, third]) {
        (Some(value), 3)
    } else if let Some(value) = lookup(tables::PAIRS, [first, second]) {
        (Some(value), 2)
    } else {
        (lookup(tables::SINGLES, [first]), 1)
    }
}
fn lookup<const N: usize>(
    table: &'static [([u16; N], &str)],
    key: [u16; N],
) -> Option<&'static str> {
    table
        .binary_search_by_key(&key, |(units, _)| *units)
        .ok()
        .map(|index| table[index].1)
}
const fn geminate(next: u16) -> &'static str {
    match next {
        0x30ab | 0x30ad | 0x30af | 0x30b1 | 0x30b3 => "k",
        0x30b5 | 0x30b7 | 0x30b9 | 0x30bb | 0x30bd => "s",
        0x30bf | 0x30c1 | 0x30c4 | 0x30c6 | 0x30c8 => "t",
        0x30d1 | 0x30d4 | 0x30d7 | 0x30da | 0x30dd => "p",
        _ => "",
    }
}
const fn nasal(next: u16) -> &'static str {
    match next {
        0x30d0 | 0x30d3 | 0x30d6 | 0x30d9 | 0x30dc | 0x30d1 | 0x30d4 | 0x30d7 | 0x30da | 0x30dd
        | 0x30de | 0x30df | 0x30e0 | 0x30e1 | 0x30e2 => "m",
        0x30e4 | 0x30e6 | 0x30e8 | 0x30a2 | 0x30a4 | 0x30a6 | 0x30a8 | 0x30aa => "n'",
        _ => "n",
    }
}
