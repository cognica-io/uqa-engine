//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use uqa_sql::{
    plan::{DeletePlan, InsertPlan, MergePlan, UpdatePlan},
    SQLError,
};

pub(super) fn ensure_insert(engine: &Engine, plan: &InsertPlan) -> Result<String, SQLError> {
    uqa_sql::semantics::view_privileges::ensure_insert(engine, plan)
}
pub(super) fn ensure_update(engine: &Engine, plan: &UpdatePlan) -> Result<String, SQLError> {
    uqa_sql::semantics::view_privileges::ensure_update(engine, plan)
}
pub(super) fn ensure_delete(engine: &Engine, plan: &DeletePlan) -> Result<String, SQLError> {
    uqa_sql::semantics::view_privileges::ensure_delete(engine, plan)
}
pub(super) fn ensure_merge(engine: &Engine, plan: &MergePlan) -> Result<String, SQLError> {
    uqa_sql::semantics::view_privileges::ensure_merge(engine, plan)
}
