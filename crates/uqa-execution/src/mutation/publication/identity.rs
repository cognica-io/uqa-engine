//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! What a statement knows about the identity of a document it inserts.

/// The more a statement knows about an identity, the less its publication reads. An identity that may belong to a document makes the insert a replacement, which reads the earlier version. A vacant one has no document to replace. An unused one never had a record of any kind, so its records are written without reading what they replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertedIdentity {
    /// A document may have the identity, and the insert replaces it.
    Unknown,
    /// No document has the identity in the statement's view: it was generated for the row, or it is a unique key whose conflict check found no row. An earlier document, since deleted, may have had it.
    Vacant,
    /// No document ever had the identity: it was reserved for the row above every identity in use, or it lies above the table's watermark as the statement's observation found it.
    Unused,
}

impl InsertedIdentity {
    /// Whether the insert creates its document instead of replacing one.
    pub fn is_vacant(self) -> bool {
        self != Self::Unknown
    }

    /// Whether no record of the document's identity ever existed.
    pub fn is_unused(self) -> bool {
        self == Self::Unused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_level_of_knowledge_includes_the_ones_below_it() {
        assert!(!InsertedIdentity::Unknown.is_vacant());
        assert!(!InsertedIdentity::Unknown.is_unused());
        assert!(InsertedIdentity::Vacant.is_vacant());
        assert!(!InsertedIdentity::Vacant.is_unused());
        assert!(InsertedIdentity::Unused.is_vacant());
        assert!(InsertedIdentity::Unused.is_unused());
    }
}
