//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

/// Built-in expression types without a stored-column representation. `ColumnType::AnyArray` retains its original persisted identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum ExpressionPseudoType {
    Unknown = 705,
    Any = 2276,
    AnyElement = 2283,
    AnyNonArray = 2776,
    AnyEnum = 3500,
    AnyRange = 3831,
    AnyMultiRange = 4537,
    AnyCompatibleMultiRange = 4538,
    AnyCompatible = 5077,
    AnyCompatibleArray = 5078,
    AnyCompatibleNonArray = 5079,
    AnyCompatibleRange = 5080,
}

impl ExpressionPseudoType {
    pub const ALL: [Self; 12] = [
        Self::Unknown,
        Self::Any,
        Self::AnyElement,
        Self::AnyNonArray,
        Self::AnyEnum,
        Self::AnyRange,
        Self::AnyMultiRange,
        Self::AnyCompatibleMultiRange,
        Self::AnyCompatible,
        Self::AnyCompatibleArray,
        Self::AnyCompatibleNonArray,
        Self::AnyCompatibleRange,
    ];

    pub fn from_oid(oid: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|ty| *ty as u32 == oid)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        if name != "unknown" && !name.starts_with("any") {
            return None;
        }
        Self::ALL.into_iter().find(|ty| ty.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Any => "any",
            Self::AnyElement => "anyelement",
            Self::AnyNonArray => "anynonarray",
            Self::AnyEnum => "anyenum",
            Self::AnyRange => "anyrange",
            Self::AnyMultiRange => "anymultirange",
            Self::AnyCompatibleMultiRange => "anycompatiblemultirange",
            Self::AnyCompatible => "anycompatible",
            Self::AnyCompatibleArray => "anycompatiblearray",
            Self::AnyCompatibleNonArray => "anycompatiblenonarray",
            Self::AnyCompatibleRange => "anycompatiblerange",
        }
    }
}
