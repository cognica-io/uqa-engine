//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::StorageUpgradeEvent;

/// Receives what an open does to bring a database to the current native
/// mapping.
///
/// The open calls [`observe`](Self::observe) on the thread that opens the
/// database, between its statements and, for
/// [`StorageUpgradeEvent::StepAdvanced`], while one of them runs. The open
/// holds the write transaction of the database for all of it: an observer
/// returns quickly and neither opens nor queries that database.
pub trait StorageUpgradeObserver: Send + Sync {
    /// Called once for every event, in the order the open produces them.
    fn observe(&self, event: &StorageUpgradeEvent);
}
