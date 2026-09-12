//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable Nori POS vocabulary; ordinal values differ from Lucene tag codes.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::nori::error::invalid;
use crate::nori::DictionaryResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[repr(u8)]
pub enum POSType {
    Morpheme,
    Compound,
    Inflect,
    Preanalysis,
}

impl POSType {
    pub(crate) fn from_ordinal(value: u8) -> DictionaryResult<Self> {
        match value {
            0 => Ok(Self::Morpheme),
            1 => Ok(Self::Compound),
            2 => Ok(Self::Inflect),
            3 => Ok(Self::Preanalysis),
            _ => Err(invalid("word entries", "invalid POS type")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Morpheme => "MORPHEME",
            Self::Compound => "COMPOUND",
            Self::Inflect => "INFLECT",
            Self::Preanalysis => "PREANALYSIS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct POSTag(u8);

impl POSTag {
    pub const EP: Self = Self(0);
    pub const EF: Self = Self(1);
    pub const EC: Self = Self(2);
    pub const ETN: Self = Self(3);
    pub const ETM: Self = Self(4);
    pub const IC: Self = Self(5);
    pub const JKS: Self = Self(6);
    pub const JKC: Self = Self(7);
    pub const JKG: Self = Self(8);
    pub const JKO: Self = Self(9);
    pub const JKB: Self = Self(10);
    pub const JKV: Self = Self(11);
    pub const JKQ: Self = Self(12);
    pub const JX: Self = Self(13);
    pub const JC: Self = Self(14);
    pub const MAG: Self = Self(15);
    pub const MAJ: Self = Self(16);
    pub const MM: Self = Self(17);
    pub const NNG: Self = Self(18);
    pub const NNP: Self = Self(19);
    pub const NNB: Self = Self(20);
    pub const NNBC: Self = Self(21);
    pub const NP: Self = Self(22);
    pub const NR: Self = Self(23);
    pub const SF: Self = Self(24);
    pub const SH: Self = Self(25);
    pub const SL: Self = Self(26);
    pub const SN: Self = Self(27);
    pub const SP: Self = Self(28);
    pub const SSC: Self = Self(29);
    pub const SSO: Self = Self(30);
    pub const SC: Self = Self(31);
    pub const SY: Self = Self(32);
    pub const SE: Self = Self(33);
    pub const VA: Self = Self(34);
    pub const VCN: Self = Self(35);
    pub const VCP: Self = Self(36);
    pub const VV: Self = Self(37);
    pub const VX: Self = Self(38);
    pub const XPN: Self = Self(39);
    pub const XR: Self = Self(40);
    pub const XSA: Self = Self(41);
    pub const XSN: Self = Self(42);
    pub const XSV: Self = Self(43);
    pub const UNKNOWN: Self = Self(44);
    pub const UNA: Self = Self(45);
    pub const NA: Self = Self(46);
    pub const VSV: Self = Self(47);
    pub const NAMES: &'static [&'static str] = &[
        "EP", "EF", "EC", "ETN", "ETM", "IC", "JKS", "JKC", "JKG", "JKO", "JKB", "JKV", "JKQ",
        "JX", "JC", "MAG", "MAJ", "MM", "NNG", "NNP", "NNB", "NNBC", "NP", "NR", "SF", "SH", "SL",
        "SN", "SP", "SSC", "SSO", "SC", "SY", "SE", "VA", "VCN", "VCP", "VV", "VX", "XPN", "XR",
        "XSA", "XSN", "XSV", "UNKNOWN", "UNA", "NA", "VSV",
    ];
    const CODES: &'static [i16] = &[
        100, 101, 102, 103, 104, 110, 120, 121, 122, 123, 124, 125, 126, 127, 128, 130, 131, 140,
        150, 151, 152, 153, 154, 155, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171,
        172, 173, 174, 181, 182, 183, 184, 185, 999, -1, -1, -1,
    ];

    pub fn name(self) -> &'static str {
        Self::NAMES[self.0 as usize]
    }
    pub fn code(self) -> i16 {
        Self::CODES[self.0 as usize]
    }
    pub fn ordinal(self) -> u8 {
        self.0
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .map(|index| Self(index as u8))
    }

    pub(crate) fn from_ordinal(value: u8) -> DictionaryResult<Self> {
        if value as usize >= Self::NAMES.len() {
            return Err(invalid("word entries", "invalid POS tag"));
        }
        Ok(Self(value))
    }
}

impl Serialize for POSTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for POSTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::from_name(&name)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown Nori POS tag: {name}")))
    }
}
