//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Where an inserted row's physical document identity comes from.

/// Where an inserted row's physical document identity comes from, as its table's columns decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    /// The table generates the identity: a declared table without a single primary-key column, or whose key is not an integer.
    Generated,
    /// The table's single integer primary-key column names the identity. Where the table maps its keys, a key in `0..KEY_IDENTITY_LIMIT` is the identity; any other row takes an identity the table generates at or above that limit, which no key names.
    IntegerKey,
    /// The `id` field of a table without declared columns names the identity, as the document API does.
    Document,
}

impl IdentitySource {
    /// Whether a value the row supplies can name its identity.
    #[must_use]
    pub const fn accepts_supplied(self) -> bool {
        !matches!(self, Self::Generated)
    }
}
