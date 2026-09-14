//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pinned small-kana replacements preserve raw units and contract only the Ainu /pu/ pair.

use crate::morphology::filter::Work;
use crate::AnalysisResult;

#[derive(Debug, Clone, Copy)]
pub(in crate::kuromoji) enum Kana {
    Hiragana,
    Katakana,
}

impl Kana {
    pub(super) fn expand(self, term: &mut [u16], work: &mut Work<'_>) -> AnalysisResult<usize> {
        let mut read = 0;
        let mut write = 0;
        while read < term.len() {
            work.tick()?;
            let unit = term[read];
            term[write] = if matches!(self, Self::Katakana)
                && unit == 0x31f7
                && term.get(read + 1) == Some(&0x309a)
            {
                read += 1;
                0x30d7
            } else {
                self.map(unit)
            };
            read += 1;
            write += 1;
        }
        Ok(write)
    }

    const fn map(self, unit: u16) -> u16 {
        match self {
            Self::Hiragana => match unit {
                0x3041 => 0x3042,
                0x3043 => 0x3044,
                0x3045 => 0x3046,
                0x3047 => 0x3048,
                0x3049 => 0x304a,
                0x3063 => 0x3064,
                0x3083 => 0x3084,
                0x3085 => 0x3086,
                0x3087 => 0x3088,
                0x308e => 0x308f,
                0x3095 => 0x304b,
                0x3096 => 0x3051,
                _ => unit,
            },
            Self::Katakana => match unit {
                0x30a1 => 0x30a2,
                0x30a3 => 0x30a4,
                0x30a5 => 0x30a6,
                0x30a7 => 0x30a8,
                0x30a9 => 0x30aa,
                0x30f5 => 0x30ab,
                0x31f0 => 0x30af,
                0x30f6 => 0x30b1,
                0x31f1 => 0x30b7,
                0x31f2 => 0x30b9,
                0x30c3 => 0x30c4,
                0x31f3 => 0x30c8,
                0x31f4 => 0x30cc,
                0x31f5 => 0x30cf,
                0x31f6 => 0x30d2,
                0x31f7 => 0x30d5,
                0x31f8 => 0x30d8,
                0x31f9 => 0x30db,
                0x31fa => 0x30e0,
                0x30e3 => 0x30e4,
                0x30e5 => 0x30e6,
                0x30e7 => 0x30e8,
                0x31fb => 0x30e9,
                0x31fc => 0x30ea,
                0x31fd => 0x30eb,
                0x31fe => 0x30ec,
                0x31ff => 0x30ed,
                0x30ee => 0x30ef,
                _ => unit,
            },
        }
    }
}
