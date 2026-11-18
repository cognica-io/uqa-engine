//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation fields share retained construction and keep completed scalar prefixes owned through later callbacks.

use super::{Copying, Result};

mod conflict;
mod input;
mod merge;
mod targets;
#[cfg(test)]
mod tests;

pub(super) use conflict::construct_conflict;
pub(super) use input::construct_assignments;
pub(super) use merge::construct_merge_whens;
pub(super) use targets::construct_targets;
