//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lower explicit relation locks without binding names before execution.

use super::{range_var_name, NodeEnum, Result, SQLError, Statement};
use crate::ast::{LockTableStmt, LockTableTarget, TableLockMode};

pub(super) fn compile_lock_table(lock: &pg_query::protobuf::LockStmt) -> Result<Statement> {
    let mode = match lock.mode {
        1 => TableLockMode::AccessShare,
        2 => TableLockMode::RowShare,
        3 => TableLockMode::RowExclusive,
        4 => TableLockMode::ShareUpdateExclusive,
        5 => TableLockMode::Share,
        6 => TableLockMode::ShareRowExclusive,
        7 => TableLockMode::Exclusive,
        8 => TableLockMode::AccessExclusive,
        _ => return Err(SQLError::Internal("invalid parsed table-lock mode".into())),
    };
    let targets = lock
        .relations
        .iter()
        .map(|node| {
            let Some(NodeEnum::RangeVar(relation)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "invalid parsed table-lock target".into(),
                ));
            };
            Ok(LockTableTarget {
                name: range_var_name(relation),
                include_descendants: relation.inh,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Statement::LockTable(LockTableStmt {
        targets,
        mode,
        nowait: lock.nowait,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_table_locks_preserve_modes_names_order_and_inheritance() {
        for (phrase, mode) in [
            ("ACCESS SHARE", TableLockMode::AccessShare),
            ("ROW SHARE", TableLockMode::RowShare),
            ("ROW EXCLUSIVE", TableLockMode::RowExclusive),
            (
                "SHARE UPDATE EXCLUSIVE",
                TableLockMode::ShareUpdateExclusive,
            ),
            ("SHARE", TableLockMode::Share),
            ("SHARE ROW EXCLUSIVE", TableLockMode::ShareRowExclusive),
            ("EXCLUSIVE", TableLockMode::Exclusive),
            ("ACCESS EXCLUSIVE", TableLockMode::AccessExclusive),
        ] {
            let sql = format!("LOCK TABLE ONLY \"Odd.Schema\".\"Odd.Name\", ONLY (other), parent * IN {phrase} MODE NOWAIT");
            let Statement::LockTable(lock) = crate::compile(&sql).unwrap().remove(0) else {
                panic!("expected LOCK TABLE")
            };
            assert_eq!(lock.mode, mode);
            assert!(lock.nowait);
            assert_eq!(
                lock.targets,
                vec![
                    LockTableTarget {
                        name: "\"Odd.Schema\".\"Odd.Name\"".into(),
                        include_descendants: false
                    },
                    LockTableTarget {
                        name: "other".into(),
                        include_descendants: false
                    },
                    LockTableTarget {
                        name: "parent".into(),
                        include_descendants: true
                    },
                ]
            );
            let plan = crate::plan::UnifiedPlan::lower(Statement::LockTable(lock));
            assert!(!crate::semantics::effects::read_only::plan_sets_transaction_snapshot(&plan));
        }
        let Statement::LockTable(lock) = crate::compile("LOCK t").unwrap().remove(0) else {
            panic!("expected LOCK")
        };
        assert_eq!(lock.mode, TableLockMode::AccessExclusive);
        assert!(!lock.nowait);
        assert!(lock.targets[0].include_descendants);
    }
}
