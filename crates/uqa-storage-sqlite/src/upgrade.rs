//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! What an open does to bring a database to the current native mapping,
//! for the caller of the open.
//!
//! An open of a database that is stored in an earlier mapping upgrades it
//! before it returns an engine. Some of that work writes rows of the
//! database again and so grows with the database: the upgrade to mapping 18
//! wrote the lookup key of every B-tree index entry, which took seconds for
//! one database and eight minutes for another. A caller that supervises the
//! open has to tell such work from a hang, and an operator has to know what
//! a release will do to a database before it does it.
//!
//! - [`inspect_storage_upgrade`] reads, without changing the file, which
//!   mapping a database is stored in and which steps an open would run.
//! - A [`StorageUpgradeObserver`] is told the steps of the upgrade as an
//!   open runs them.

mod event;
mod inspection;
mod observer;
mod report;
mod step;

pub use event::StorageUpgradeEvent;
pub use inspection::{inspect_storage_upgrade, StorageUpgradeInspection};
pub use observer::StorageUpgradeObserver;
pub(crate) use report::UpgradeReport;
pub use step::StorageUpgradeStep;
