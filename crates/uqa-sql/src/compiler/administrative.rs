//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session control, ANALYZE, EXPLAIN, TRUNCATE, and transactions.

use super::dispatch::compile_stmt;
use super::{
    range_var_name, render_relation_component, NodeEnum, Result, SQLError, Statement,
    TransactionStmt,
};

fn compile_vacuum_option_value(node: &NodeEnum) -> Result<crate::ast::VacuumOptionValue> {
    use crate::ast::VacuumOptionValue;

    match node {
        NodeEnum::Boolean(value) => Ok(VacuumOptionValue::Boolean(value.boolval)),
        NodeEnum::Integer(value) => Ok(VacuumOptionValue::Integer(value.ival)),
        NodeEnum::String(value) => Ok(VacuumOptionValue::String(value.sval.clone())),
        NodeEnum::Float(value) => Ok(VacuumOptionValue::String(value.fval.clone())),
        NodeEnum::AConst(value) => match value.val.as_ref() {
            Some(pg_query::protobuf::a_const::Val::Boolval(value)) => {
                Ok(VacuumOptionValue::Boolean(value.boolval))
            }
            Some(pg_query::protobuf::a_const::Val::Ival(value)) => {
                Ok(VacuumOptionValue::Integer(value.ival))
            }
            Some(pg_query::protobuf::a_const::Val::Sval(value)) => {
                Ok(VacuumOptionValue::String(value.sval.clone()))
            }
            Some(pg_query::protobuf::a_const::Val::Fval(value)) => {
                Ok(VacuumOptionValue::String(value.fval.clone()))
            }
            _ => Err(SQLError::Internal(
                "VACUUM contains a malformed option value".into(),
            )),
        },
        _ => Err(SQLError::Internal(
            "VACUUM contains a malformed option value".into(),
        )),
    }
}

fn compile_vacuum(stmt: &pg_query::protobuf::VacuumStmt) -> Result<Statement> {
    use crate::ast::{VacuumOption, VacuumStmt, VacuumTarget};

    let mut options = Vec::with_capacity(stmt.options.len());
    for node in &stmt.options {
        let Some(NodeEnum::DefElem(option)) = node.node.as_ref() else {
            return Err(SQLError::Internal(
                "VACUUM contains a malformed option".into(),
            ));
        };
        let value = option
            .arg
            .as_ref()
            .and_then(|argument| argument.node.as_ref())
            .map(compile_vacuum_option_value)
            .transpose()?;
        options.push(VacuumOption {
            name: option.defname.to_ascii_lowercase(),
            value,
        });
    }

    let mut targets = Vec::with_capacity(stmt.rels.len());
    for node in &stmt.rels {
        let Some(NodeEnum::VacuumRelation(target)) = node.node.as_ref() else {
            return Err(SQLError::Internal(
                "VACUUM contains a malformed relation".into(),
            ));
        };
        if target.oid != 0 {
            return Err(SQLError::Internal(
                "VACUUM contains an unexpected relation OID".into(),
            ));
        }
        let relation = target.relation.as_ref().ok_or_else(|| {
            SQLError::Internal("VACUUM relation is missing its table name".into())
        })?;
        if relation.relname.is_empty() {
            return Err(SQLError::Internal(
                "VACUUM relation has an empty table name".into(),
            ));
        }
        let mut columns = Vec::with_capacity(target.va_cols.len());
        for column in &target.va_cols {
            let Some(NodeEnum::String(column)) = column.node.as_ref() else {
                return Err(SQLError::Internal(
                    "VACUUM contains a malformed column name".into(),
                ));
            };
            columns.push(column.sval.clone());
        }
        targets.push(VacuumTarget {
            catalog: (!relation.catalogname.is_empty()).then(|| relation.catalogname.clone()),
            table: range_var_name(relation),
            include_descendants: relation.inh,
            columns,
        });
    }

    Ok(Statement::Vacuum(VacuumStmt { options, targets }))
}

fn transaction_characteristics(
    args: &[pg_query::protobuf::Node],
) -> Result<crate::ast::TransactionCharacteristics> {
    use crate::ast::{TransactionCharacteristics, TransactionIsolationLevel};

    let mut characteristics = TransactionCharacteristics::default();
    for arg in args {
        let Some(NodeEnum::DefElem(option)) = arg.node.as_ref() else {
            return Err(SQLError::Internal(
                "transaction mode contains a malformed option".into(),
            ));
        };
        let value = option
            .arg
            .as_ref()
            .and_then(|argument| argument.node.as_ref())
            .ok_or_else(|| SQLError::Internal("transaction mode has no value".into()))?;
        match option.defname.as_str() {
            "transaction_isolation" => {
                let NodeEnum::AConst(constant) = value else {
                    return Err(SQLError::Internal(
                        "transaction isolation has a malformed value".into(),
                    ));
                };
                let Some(pg_query::protobuf::a_const::Val::Sval(value)) = constant.val.as_ref()
                else {
                    return Err(SQLError::Internal(
                        "transaction isolation is not a string".into(),
                    ));
                };
                characteristics.isolation = Some(match value.sval.as_str() {
                    "read uncommitted" => TransactionIsolationLevel::ReadUncommitted,
                    "read committed" => TransactionIsolationLevel::ReadCommitted,
                    "repeatable read" => TransactionIsolationLevel::RepeatableRead,
                    "serializable" => TransactionIsolationLevel::Serializable,
                    other => {
                        return Err(SQLError::Internal(format!(
                            "unknown parsed transaction isolation level {other:?}"
                        )))
                    }
                });
            }
            "transaction_read_only" | "transaction_deferrable" => {
                let NodeEnum::AConst(constant) = value else {
                    return Err(SQLError::Internal(format!(
                        "{} has a malformed value",
                        option.defname
                    )));
                };
                let Some(pg_query::protobuf::a_const::Val::Ival(value)) = constant.val.as_ref()
                else {
                    return Err(SQLError::Internal(format!(
                        "{} is not a boolean integer",
                        option.defname
                    )));
                };
                let enabled = value.ival != 0;
                if option.defname == "transaction_read_only" {
                    characteristics.read_only = Some(enabled);
                } else {
                    characteristics.deferrable = Some(enabled);
                }
            }
            other => {
                return Err(SQLError::Internal(format!(
                    "unknown parsed transaction mode {other:?}"
                )))
            }
        }
    }
    Ok(characteristics)
}

pub(super) fn discard_target(mode: i32) -> Result<crate::ast::DiscardTarget> {
    use crate::ast::DiscardTarget;
    match mode {
        1 => Ok(DiscardTarget::All),
        2 => Ok(DiscardTarget::Plans),
        3 => Ok(DiscardTarget::Sequences),
        4 => Ok(DiscardTarget::Temp),
        other => Err(SQLError::Internal(format!(
            "unknown DISCARD target {other}"
        ))),
    }
}

pub(super) fn compile_analyze(stmt: &pg_query::protobuf::VacuumStmt) -> Result<Statement> {
    if stmt.is_vacuumcmd {
        return compile_vacuum(stmt);
    }

    if !stmt.options.is_empty() {
        return Err(SQLError::Unsupported(
            "ANALYZE options are not implemented".into(),
        ));
    }

    let table = match stmt.rels.as_slice() {
        [] => None,
        [node] => {
            let Some(NodeEnum::VacuumRelation(relation)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "ANALYZE contains a malformed relation".into(),
                ));
            };
            if relation.oid != 0 {
                return Err(SQLError::Unsupported(
                    "OID-targeted ANALYZE is not implemented".into(),
                ));
            }
            if !relation.va_cols.is_empty() {
                return Err(SQLError::Unsupported(
                    "ANALYZE column lists are not implemented".into(),
                ));
            }
            let range = relation.relation.as_ref().ok_or_else(|| {
                SQLError::Internal("ANALYZE relation is missing its table name".into())
            })?;
            if !range.catalogname.is_empty() {
                return Err(SQLError::Unsupported(
                    "cross-database ANALYZE is not implemented".into(),
                ));
            }
            if range.relname.is_empty() {
                return Err(SQLError::Internal(
                    "ANALYZE relation has an empty table name".into(),
                ));
            }
            Some(range_var_name(range))
        }
        _ => {
            return Err(SQLError::Unsupported(
                "ANALYZE of multiple tables is not implemented".into(),
            ));
        }
    };

    Ok(Statement::Analyze { table })
}

pub(super) fn compile_variable_set(
    stmt: &pg_query::protobuf::VariableSetStmt,
) -> Result<Statement> {
    use pg_query::protobuf::VariableSetKind;

    match stmt.kind() {
        VariableSetKind::VarReset => {
            return Ok(Statement::ResetVariable {
                name: stmt.name.clone(),
            });
        }
        VariableSetKind::VarResetAll => return Ok(Statement::ResetAllVariables),
        VariableSetKind::VarSetDefault => {
            return Ok(Statement::ResetVariable {
                name: stmt.name.clone(),
            });
        }
        _ => {}
    }

    if matches!(stmt.kind(), VariableSetKind::VarSetMulti) {
        return match stmt.name.as_str() {
            "TRANSACTION" => Ok(Statement::Transaction(TransactionStmt::SetCharacteristics(
                transaction_characteristics(&stmt.args)?,
            ))),
            "SESSION CHARACTERISTICS" => Ok(Statement::Transaction(
                TransactionStmt::SetSessionCharacteristics(transaction_characteristics(
                    &stmt.args,
                )?),
            )),
            "TRANSACTION SNAPSHOT" => {
                let [argument] = stmt.args.as_slice() else {
                    return Err(SQLError::Internal(
                        "SET TRANSACTION SNAPSHOT requires one value".into(),
                    ));
                };
                let Some(NodeEnum::AConst(constant)) = argument.node.as_ref() else {
                    return Err(SQLError::Internal(
                        "SET TRANSACTION SNAPSHOT has a malformed value".into(),
                    ));
                };
                let Some(pg_query::protobuf::a_const::Val::Sval(value)) = constant.val.as_ref()
                else {
                    return Err(SQLError::Internal(
                        "SET TRANSACTION SNAPSHOT value is not a string".into(),
                    ));
                };
                Ok(Statement::Transaction(TransactionStmt::SetSnapshot(
                    value.sval.clone(),
                )))
            }
            other => Err(SQLError::Internal(format!(
                "unknown parsed SET MULTI target {other:?}"
            ))),
        };
    }

    // Capture each argument as a string and join with commas. PG's
    // SET search_path TO a, b, c arrives as a list of A_Const nodes.
    let mut parts: Vec<String> = Vec::new();
    for arg in &stmt.args {
        let node = arg
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SET contains an empty argument".into()))?;
        match node {
            NodeEnum::AConst(constant) => match constant.val.as_ref() {
                Some(pg_query::protobuf::a_const::Val::Sval(value)) => {
                    parts.push(value.sval.clone());
                }
                Some(pg_query::protobuf::a_const::Val::Ival(value)) => {
                    parts.push(value.ival.to_string());
                }
                Some(pg_query::protobuf::a_const::Val::Fval(value)) => {
                    parts.push(value.fval.clone());
                }
                Some(pg_query::protobuf::a_const::Val::Boolval(value)) => {
                    parts.push(value.boolval.to_string());
                }
                None if constant.isnull => parts.push("NULL".into()),
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "SET argument {other:?} is not supported"
                    )));
                }
            },
            NodeEnum::TypeCast(cast) => {
                let Some(NodeEnum::AConst(constant)) = cast
                    .arg
                    .as_ref()
                    .and_then(|argument| argument.node.as_ref())
                else {
                    return Err(SQLError::Unsupported(
                        "SET type-cast argument must contain a literal".into(),
                    ));
                };
                let Some(pg_query::protobuf::a_const::Val::Sval(value)) = constant.val.as_ref()
                else {
                    return Err(SQLError::Unsupported(
                        "SET type-cast argument must contain a string literal".into(),
                    ));
                };
                parts.push(value.sval.clone());
            }
            NodeEnum::String(value) => parts.push(value.sval.clone()),
            other => {
                return Err(SQLError::Unsupported(format!(
                    "SET argument {other:?} is not supported"
                )));
            }
        }
    }
    let value = if stmt.name.eq_ignore_ascii_case("search_path") {
        parts
            .iter()
            .map(|part| render_relation_component(part))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        parts.join(",")
    };
    Ok(Statement::SetVariable {
        name: stmt.name.clone(),
        value,
    })
}

pub(super) fn compile_set_constraints(
    stmt: &pg_query::protobuf::ConstraintsSetStmt,
) -> Result<Statement> {
    let mut constraints = Vec::with_capacity(stmt.constraints.len());
    for constraint in &stmt.constraints {
        let Some(NodeEnum::RangeVar(name)) = constraint.node.as_ref() else {
            return Err(SQLError::Internal(
                "SET CONSTRAINTS contains a malformed constraint name".into(),
            ));
        };
        constraints.push(crate::ast::SetConstraintName {
            catalog: (!name.catalogname.is_empty()).then(|| name.catalogname.clone()),
            schema: (!name.schemaname.is_empty()).then(|| name.schemaname.clone()),
            name: name.relname.clone(),
        });
    }
    Ok(Statement::SetConstraints {
        constraints,
        deferred: stmt.deferred,
    })
}

pub(super) fn compile_explain(stmt: &pg_query::protobuf::ExplainStmt) -> Result<Statement> {
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("EXPLAIN without body".into()))?;
    let mut analyze = false;
    let mut verbose = false;
    let mut format: Option<String> = None;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "EXPLAIN contains a malformed option".into(),
            ));
        };
        let name = elem.defname.to_ascii_lowercase();
        match name.as_str() {
            "analyze" => analyze = compile_explain_bool_option(elem, "ANALYZE")?,
            "verbose" => verbose = compile_explain_bool_option(elem, "VERBOSE")?,
            "format" => {
                if let Some(NodeEnum::String(s)) = elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    format = Some(s.sval.clone());
                } else {
                    return Err(SQLError::TypeMismatch(
                        "EXPLAIN FORMAT expects a format name".into(),
                    ));
                }
            }
            _ => {
                return Err(SQLError::Unsupported(format!(
                    "EXPLAIN option `{name}` is not supported"
                )));
            }
        }
    }
    let inner = compile_stmt(body)?;
    Ok(Statement::Explain {
        analyze,
        verbose,
        format,
        body: Box::new(inner),
    })
}

pub(super) fn compile_explain_bool_option(
    elem: &pg_query::protobuf::DefElem,
    name: &str,
) -> Result<bool> {
    let Some(argument) = elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    else {
        return Ok(true);
    };
    match argument {
        NodeEnum::Boolean(value) => Ok(value.boolval),
        NodeEnum::Integer(value) => Ok(value.ival != 0),
        NodeEnum::String(value) => match value.sval.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Ok(true),
            "false" | "off" | "no" | "0" => Ok(false),
            _ => Err(SQLError::TypeMismatch(format!(
                "EXPLAIN {name} expects a boolean value"
            ))),
        },
        _ => Err(SQLError::TypeMismatch(format!(
            "EXPLAIN {name} expects a boolean value"
        ))),
    }
}

pub(super) fn compile_truncate(stmt: &pg_query::protobuf::TruncateStmt) -> Result<Statement> {
    let mut tables = Vec::new();
    for relation in &stmt.relations {
        let Some(NodeEnum::RangeVar(range)) = relation.node.as_ref() else {
            return Err(SQLError::Internal(
                "TRUNCATE contains a malformed table target".into(),
            ));
        };
        tables.push(crate::ast::TruncateTarget {
            table: range_var_name(range),
            include_descendants: range.inh,
        });
    }
    if tables.is_empty() {
        return Err(SQLError::Internal("TRUNCATE without a table".into()));
    }
    let cascade = matches!(
        stmt.behavior(),
        pg_query::protobuf::DropBehavior::DropCascade
    );
    Ok(Statement::Truncate {
        tables,
        cascade,
        restart_identity: stmt.restart_seqs,
    })
}

pub(super) fn compile_transaction(stmt: &pg_query::protobuf::TransactionStmt) -> Result<Statement> {
    use pg_query::protobuf::TransactionStmtKind;
    let kind = match stmt.kind() {
        TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart => {
            let characteristics = transaction_characteristics(&stmt.options)?;
            if characteristics == crate::ast::TransactionCharacteristics::default() {
                TransactionStmt::Begin
            } else {
                TransactionStmt::BeginWithCharacteristics(characteristics)
            }
        }
        TransactionStmtKind::TransStmtCommit => {
            if stmt.chain {
                TransactionStmt::CommitAndChain
            } else {
                TransactionStmt::Commit
            }
        }
        TransactionStmtKind::TransStmtRollback => {
            if stmt.chain {
                TransactionStmt::RollbackAndChain
            } else {
                TransactionStmt::Rollback
            }
        }
        TransactionStmtKind::TransStmtSavepoint => {
            TransactionStmt::Savepoint(stmt.savepoint_name.clone())
        }
        TransactionStmtKind::TransStmtRelease => {
            TransactionStmt::ReleaseSavepoint(stmt.savepoint_name.clone())
        }
        TransactionStmtKind::TransStmtRollbackTo => {
            TransactionStmt::RollbackToSavepoint(stmt.savepoint_name.clone())
        }
        other => {
            return Err(SQLError::Unsupported(format!("transaction kind {other:?}")));
        }
    };
    Ok(Statement::Transaction(kind))
}
