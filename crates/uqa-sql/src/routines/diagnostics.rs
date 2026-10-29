//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine diagnostic frames use declaration identities, independently of SQLERRM.

use crate::ast::CreateFunction;

mod production;
pub use production::sql_context_with_control;

pub enum SQLRoutineStage {
    Inlining,
    Startup,
    Statement(usize),
}

pub fn sql_context(definition: &CreateFunction, stage: SQLRoutineStage) -> String {
    sql_context_with_control(
        definition,
        stage,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary routine context")
    .into_uncontrolled()
    .expect("ordinary routine context has no reservation")
}
