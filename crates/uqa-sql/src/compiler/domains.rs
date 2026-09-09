//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain type and constraint declarations.

use pg_query::protobuf::{ConstrType, CreateDomainStmt};

use super::{
    compile_expr, compile_pg_type_name, extract_string, render_relation_component, NodeEnum,
    Result, SQLError,
};
use crate::ast::{CreateDomain, DomainCheck, DomainNotNull};

pub(super) fn compile_create_domain(statement: &CreateDomainStmt) -> Result<CreateDomain> {
    let name = qualified_name(&statement.domainname)?;
    let base = compile_pg_type_name(
        statement
            .type_name
            .as_ref()
            .ok_or_else(|| SQLError::Internal("domain declaration has no base type".into()))?,
        &name,
    )?;
    let collation = statement
        .coll_clause
        .as_ref()
        .map(|clause| qualified_name(&clause.collname))
        .transpose()?;
    let mut definition = CreateDomain {
        name,
        base,
        collation,
        default: None,
        not_null: None,
        checks: Vec::new(),
    };
    let mut nullability = None;
    for node in &statement.constraints {
        let Some(NodeEnum::Constraint(constraint)) = node.node.as_ref() else {
            return Err(SQLError::Internal(
                "domain declaration contains a malformed constraint".into(),
            ));
        };
        let name = (!constraint.conname.is_empty()).then(|| constraint.conname.clone());
        match constraint.contype() {
            ConstrType::ConstrDefault => {
                if definition.default.is_some() {
                    return Err(domain_syntax_error("multiple default values specified"));
                }
                definition.default = Some(compile_expr(constraint.raw_expr.as_ref().ok_or_else(
                    || SQLError::Internal("domain default has no expression".into()),
                )?)?);
            }
            ConstrType::ConstrNotnull | ConstrType::ConstrNull => {
                let not_null = constraint.contype() == ConstrType::ConstrNotnull;
                if nullability.is_some_and(|previous| previous != not_null) {
                    return Err(domain_syntax_error("conflicting NULL/NOT NULL constraints"));
                }
                nullability = Some(not_null);
                if not_null {
                    definition.not_null = Some(DomainNotNull { name });
                }
            }
            ConstrType::ConstrCheck => {
                definition.checks.push(DomainCheck {
                    name,
                    expression: compile_expr(constraint.raw_expr.as_ref().ok_or_else(|| {
                        SQLError::Internal("domain check has no expression".into())
                    })?)?,
                });
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "domain constraint {other:?}"
                )))
            }
        }
    }
    Ok(definition)
}

fn qualified_name(nodes: &[pg_query::protobuf::Node]) -> Result<String> {
    let names = nodes
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    if names.is_empty() || names.len() > 2 {
        return Err(domain_syntax_error("improper qualified name"));
    }
    Ok(names
        .iter()
        .map(|name| render_relation_component(name))
        .collect::<Vec<_>>()
        .join("."))
}

fn domain_syntax_error(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: message.into(),
    }
}
