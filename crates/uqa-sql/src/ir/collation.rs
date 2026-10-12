//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed collation selection retained by executable expression rewrites.

use crate::catalog::collations::derivation::Derivation;
use crate::{ColumnType, SQLError};

/// An analyzed result keeps its coercibility as well as its OID: an explicit selection outranks implicit inputs, and an indeterminate selection retains the conflicting identities for later key diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ResultCollation {
    derivation: Derivation,
    initial_oid: u32,
}

impl<'de> serde::Deserialize<'de> for ResultCollation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Stored {
            derivation: Derivation,
            initial_oid: u32,
        }
        let Stored {
            derivation,
            initial_oid,
        } = <Stored as serde::Deserialize>::deserialize(deserializer)?;
        let valid_oid = |oid| u32::try_from(oid).is_ok();
        let valid = match derivation {
            Derivation::None => true,
            Derivation::Implicit(oid) | Derivation::Explicit(oid) => valid_oid(oid),
            Derivation::Conflict(left, right) => valid_oid(left) && valid_oid(right),
            Derivation::InvalidExplicit(..) | Derivation::InvalidImplicit(..) => false,
        };
        if !valid {
            return Err(serde::de::Error::custom(
                "invalid retained collation selection",
            ));
        }
        Ok(Self::new(derivation, initial_oid))
    }
}

impl ResultCollation {
    pub(crate) fn new(derivation: Derivation, initial_oid: u32) -> Self {
        Self {
            derivation,
            initial_oid,
        }
    }

    pub(crate) fn derivation(self) -> Derivation {
        self.derivation
    }

    pub(crate) fn initial_oid(self) -> u32 {
        self.initial_oid
    }

    pub fn oid(self) -> u32 {
        u32::try_from(self.derivation.oid()).expect("analyzed result collation OID")
    }

    /// A typed constant can directly carry an implicit selection only when its pre-assignment field collation is the same.
    pub fn implicit_oid(self) -> Option<u32> {
        match self.derivation {
            Derivation::Implicit(_) if self.initial_oid == self.oid() => Some(self.oid()),
            _ => None,
        }
    }

    pub(crate) fn validate(self, ty: &ColumnType) -> Result<(), SQLError> {
        self.derivation.validate()?;
        crate::catalog::collations::expression::validate_literal_collation(
            Some(ty),
            Some(self.oid()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_collation_codec_preserves_coercibility_and_rejects_invalid_identities() {
        for derivation in [
            Derivation::None,
            Derivation::Implicit(950),
            Derivation::Explicit(100),
            Derivation::Conflict(950, 951),
        ] {
            let selection = ResultCollation::new(derivation, 0);
            let encoded = serde_json::to_string(&selection).unwrap();
            let restored: ResultCollation = serde_json::from_str(&encoded).unwrap();
            assert_eq!(restored, selection);
            assert_eq!(restored.initial_oid(), 0);
            assert!(restored.implicit_oid().is_none());
        }
        assert_eq!(
            ResultCollation::new(Derivation::Implicit(950), 950).implicit_oid(),
            Some(950)
        );
        for derivation in [
            Derivation::Implicit(-1),
            Derivation::Explicit(i64::from(u32::MAX) + 1),
            Derivation::Conflict(950, -1),
            Derivation::InvalidExplicit(950, 951),
            Derivation::InvalidImplicit(950, 951),
        ] {
            let invalid = ResultCollation::new(derivation, 0);
            assert!(serde_json::from_str::<ResultCollation>(
                &serde_json::to_string(&invalid).unwrap()
            )
            .is_err());
        }
    }
}
