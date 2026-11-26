//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    compile_grant_table_privileges, compile_role_list, extract_string, NodeEnum, Result, SQLError,
    Statement,
};
use crate::ast::{AlterDefaultPrivilegesStmt, DefaultAclObject};
use pg_query::protobuf::{AlterDefaultPrivilegesStmt as RawStatement, ObjectType};

pub(in crate::compiler) fn compile_default_privileges(
    statement: &RawStatement,
) -> Result<Statement> {
    let action = statement
        .action
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER DEFAULT PRIVILEGES has no action".into()))?;
    let mut roles = None;
    let mut namespaces = None;
    for option in &statement.options {
        let Some(NodeEnum::DefElem(element)) = option.node.as_ref() else {
            return Err(SQLError::Internal(
                "invalid default privilege option".into(),
            ));
        };
        let Some(NodeEnum::List(values)) =
            element.arg.as_deref().and_then(|node| node.node.as_ref())
        else {
            return Err(SQLError::Internal(
                "invalid default privilege target list".into(),
            ));
        };
        let slot = match element.defname.as_str() {
            "roles" => &mut roles,
            "schemas" => &mut namespaces,
            name => {
                return Err(SQLError::Internal(format!(
                    "unrecognized default privilege option {name}"
                )))
            }
        };
        if slot.replace(&values.items).is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "conflicting or redundant options".into(),
            });
        }
    }
    let object = match action.objtype() {
        ObjectType::ObjectTable => DefaultAclObject::Table,
        ObjectType::ObjectSequence => DefaultAclObject::Sequence,
        ObjectType::ObjectFunction | ObjectType::ObjectRoutine => DefaultAclObject::Routine,
        ObjectType::ObjectType => DefaultAclObject::Type,
        ObjectType::ObjectSchema => DefaultAclObject::Schema,
        ObjectType::ObjectLargeobject => DefaultAclObject::LargeObject,
        kind => {
            return Err(SQLError::Internal(format!(
                "unrecognized default privilege object {kind:?}"
            )))
        }
    };
    Ok(Statement::AlterDefaultPrivileges(
        AlterDefaultPrivilegesStmt {
            owners: roles.map_or_else(
                || Ok(Vec::new()),
                |items| compile_role_list(items, "ALTER DEFAULT PRIVILEGES"),
            )?,
            schemas: namespaces
                .into_iter()
                .flatten()
                .map(extract_string)
                .collect::<Result<_>>()?,
            object,
            grantees: super::compile_acl_grantees(action)?,
            privileges: compile_grant_table_privileges(action)?,
            is_grant: action.is_grant,
            grant_option: action.grant_option,
        },
    ))
}
