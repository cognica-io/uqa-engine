//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View definition transactions acquire relation locks before opening a physical writer.

use crate::row_locks::binding::RelationLockSession;
use uqa_sql::SQLError;

pub trait ViewDefinitionSession: RelationLockSession {
    fn prepare_definition_write(&self) -> Result<(), SQLError>;
}
