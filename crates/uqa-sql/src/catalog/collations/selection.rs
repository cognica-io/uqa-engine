//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

/// A collation that analysis selected for a call's inputs: one of the fixed built-in collations, or the indeterminate selection that `PostgreSQL` records as `InvalidOid`. It occupies one byte, so retaining it keeps a call binding within its shared expression storage bound, and it is serialized as the `PostgreSQL` collation OID, with zero for the indeterminate selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollationSelection(u8);

impl CollationSelection {
    pub const INDETERMINATE: Self = Self(0);

    /// The selection of `oid`, where zero is indeterminate; `None` for an OID that names no built-in collation.
    #[must_use]
    pub fn from_oid(oid: u32) -> Option<Self> {
        if oid == 0 {
            return Some(Self::INDETERMINATE);
        }
        super::BUILTIN_COLLATIONS
            .iter()
            .position(|(_, candidate, ..)| *candidate == i64::from(oid))
            .map(|index| Self(u8::try_from(index + 1).expect("built-in collation table index")))
    }

    /// The selected collation OID, or zero when the selection is indeterminate.
    #[must_use]
    pub fn oid(self) -> u32 {
        match self.0 {
            0 => 0,
            index => u32::try_from(super::BUILTIN_COLLATIONS[usize::from(index) - 1].1)
                .expect("built-in collation OID"),
        }
    }
}

impl serde::Serialize for CollationSelection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.oid())
    }
}

impl<'de> serde::Deserialize<'de> for CollationSelection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let oid = <u32 as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_oid(oid).ok_or_else(|| {
            serde::de::Error::custom(format!("collation {oid} is not a built-in collation"))
        })
    }
}

#[cfg(test)]
mod tests;
