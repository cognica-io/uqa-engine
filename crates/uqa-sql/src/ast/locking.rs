//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

/// One `PostgreSQL` row-locking clause, including optional `OF` targets
/// and the `NOWAIT` / `SKIP LOCKED` wait policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockingClause {
    pub strength: LockStrength,
    pub wait: LockWait,
    /// Relation names from `OF t [, ...]`. Empty means every lockable
    /// relation in the query block.
    pub relations: Vec<String>,
}

/// `PostgreSQL` row-lock strength, strongest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LockStrength {
    ForKeyShare,
    ForShare,
    ForNoKeyUpdate,
    ForUpdate,
}

impl LockStrength {
    /// SQL keyword phrase used in `PostgreSQL` error messages.
    #[must_use]
    pub const fn sql_name(self) -> &'static str {
        match self {
            Self::ForKeyShare => "FOR KEY SHARE",
            Self::ForShare => "FOR SHARE",
            Self::ForNoKeyUpdate => "FOR NO KEY UPDATE",
            Self::ForUpdate => "FOR UPDATE",
        }
    }
}

/// Wait policy for a row-locking clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LockWait {
    Block,
    SkipLocked,
    NoWait,
}
