//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `RETURNING` clause and row-image alias lowering.

use crate::ast::{Projection, ReturningAliases};

use super::{compile_projections, NodeEnum, Result, SQLError};

pub(in crate::compiler) fn compile_returning_clause(
    clause: Option<&pg_query::protobuf::ReturningClause>,
) -> Result<(Vec<Projection>, ReturningAliases)> {
    let Some(clause) = clause else {
        return Ok((Vec::new(), ReturningAliases::default()));
    };

    let mut aliases = ReturningAliases::default();
    let mut old_seen = false;
    let mut new_seen = false;
    for option_node in &clause.options {
        let Some(NodeEnum::ReturningOption(option)) = option_node.node.as_ref() else {
            return Err(SQLError::Internal(
                "RETURNING contains a malformed row-image option".into(),
            ));
        };
        if option.value.is_empty() {
            return Err(SQLError::Internal(
                "RETURNING row-image alias is empty".into(),
            ));
        }
        match option.option() {
            pg_query::protobuf::ReturningOptionKind::ReturningOptionOld => {
                if old_seen {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: "OLD cannot be specified multiple times".into(),
                    });
                }
                aliases.old.clone_from(&option.value);
                aliases.old_explicit = true;
                old_seen = true;
            }
            pg_query::protobuf::ReturningOptionKind::ReturningOptionNew => {
                if new_seen {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: "NEW cannot be specified multiple times".into(),
                    });
                }
                aliases.new.clone_from(&option.value);
                aliases.new_explicit = true;
                new_seen = true;
            }
            pg_query::protobuf::ReturningOptionKind::Undefined => {
                return Err(SQLError::Internal(
                    "RETURNING row-image option has no kind".into(),
                ));
            }
        }
    }
    // libpg_query folds unquoted identifiers to lower case and preserves quoted identifiers, so direct comparison retains PostgreSQL's quoted-name distinction.
    if aliases.old == aliases.new {
        return Err(SQLError::Routine {
            sqlstate: "42712".into(),
            message: format!("table name \"{}\" specified more than once", aliases.old),
        });
    }

    Ok((compile_projections(&clause.exprs)?, aliases))
}
