//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine diagnostic frames use declaration identities, independently of SQLERRM.

use crate::ast::CreateFunction;

pub enum SQLRoutineStage {
    Inlining,
    Startup,
    Statement(usize),
}

pub fn sql_context(definition: &CreateFunction, stage: SQLRoutineStage) -> String {
    let name = crate::compiler::parse_regobject_name(&definition.name)
        .and_then(|mut names| names.pop())
        .unwrap_or_else(|| definition.name.clone());
    let stage = match stage {
        SQLRoutineStage::Inlining => "during inlining".to_string(),
        SQLRoutineStage::Startup => "during startup".to_string(),
        SQLRoutineStage::Statement(position) => format!("statement {position}"),
    };
    format!("SQL function \"{name}\" {stage}")
}
