//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::time::Duration;

use super::StorageUpgradeStep;

/// What an open tells its observer while it brings a database to the
/// current native mapping.
///
/// An open of a database that is already stored in the current mapping
/// tells nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StorageUpgradeEvent {
    /// The database is stored in the mapping `from`, and the open is about
    /// to bring it to `to`. Nothing has been written yet.
    Started {
        /// The mapping the database is stored in.
        from: u32,
        /// The mapping of this Engine.
        to: u32,
    },
    /// A step that writes rows again begins.
    StepStarted {
        /// The step.
        step: StorageUpgradeStep,
        /// How many rows the step reads and writes again, where the
        /// database records that number. `None` where it would have to be
        /// counted by reading them.
        rows: Option<u64>,
    },
    /// The step is still at work. Told at most once a second.
    StepAdvanced {
        /// The step.
        step: StorageUpgradeStep,
        /// How many instructions `SQLite` has run for the step so far.
        instructions: u64,
    },
    /// The step is complete.
    StepFinished {
        /// The step.
        step: StorageUpgradeStep,
        /// How long the step took.
        duration: Duration,
    },
    /// Every step is complete, and the transaction that holds the whole
    /// upgrade is being committed.
    ///
    /// A database in write-ahead mode commits by copying what the upgrade
    /// wrote from the log into the database file. `SQLite` runs no
    /// instruction while it does, so nothing is told until the commit has
    /// ended, however long the disk takes.
    Committing,
    /// The upgrade is committed.
    Finished {
        /// The time since `Started`.
        duration: Duration,
    },
}
