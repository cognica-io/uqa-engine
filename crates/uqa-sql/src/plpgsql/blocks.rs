//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Block-owned datums, taken directly from `PostgreSQL`'s initializer lists.
use super::{PLpgSQLBlock, PLpgSQLFunction, PLpgSQLStmt};
use std::collections::BTreeSet;
impl PLpgSQLFunction {
    pub fn block_variable_datums(&self) -> BTreeSet<usize> {
        let mut datums = BTreeSet::new();
        visit_block(&self.action, &mut datums);
        datums
    }
}
fn visit_block(block: &PLpgSQLBlock, output: &mut BTreeSet<usize>) {
    output.extend(&block.initvarnos);
    visit_statements(&block.body, output);
    for arm in &block.exceptions {
        visit_statements(&arm.body, output);
    }
}
fn visit_statements(statements: &[PLpgSQLStmt], output: &mut BTreeSet<usize>) {
    for statement in statements {
        match statement {
            PLpgSQLStmt::Block(block) => visit_block(block, output),
            PLpgSQLStmt::If {
                then_body,
                elsifs,
                else_body,
                ..
            } => {
                visit_statements(then_body, output);
                for (_, body) in elsifs {
                    visit_statements(body, output);
                }
                if let Some(body) = else_body {
                    visit_statements(body, output);
                }
            }
            PLpgSQLStmt::Case {
                arms, else_body, ..
            } => {
                for (_, body) in arms {
                    visit_statements(body, output);
                }
                if let Some(body) = else_body {
                    visit_statements(body, output);
                }
            }
            PLpgSQLStmt::Loop { body, .. }
            | PLpgSQLStmt::While { body, .. }
            | PLpgSQLStmt::ForI { body, .. }
            | PLpgSQLStmt::ForQuery { body, .. }
            | PLpgSQLStmt::ForDynamic { body, .. }
            | PLpgSQLStmt::ForCursor { body, .. }
            | PLpgSQLStmt::ForeachArray { body, .. } => visit_statements(body, output),
            _ => {}
        }
    }
}
