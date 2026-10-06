//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain routine privilege declarations until catalog target analysis.

use super::{
    compile_acl_role_specification, compile_object_with_args, compile_role_specification,
    CompiledRoutineTarget, NodeEnum, Result, SQLError, Statement,
};
use crate::ast::{
    AlterRoutineKind, GrantRoutineItem, GrantRoutineStmt, RoutinePrivilege, RoutineRevokeBehavior,
};
use pg_query::protobuf::{DropBehavior, GrantStmt, GrantTargetType, ObjectType};

pub(super) fn compile_grant_routine(statement: &GrantStmt) -> Result<Statement> {
    let (kind, context) = match statement.objtype() {
        ObjectType::ObjectFunction => (AlterRoutineKind::Function, "FUNCTION"),
        ObjectType::ObjectProcedure => (AlterRoutineKind::Procedure, "PROCEDURE"),
        ObjectType::ObjectRoutine => (AlterRoutineKind::Routine, "ROUTINE"),
        other => {
            return Err(SQLError::Unsupported(format!(
                "GRANT/REVOKE object type {other:?} is not supported"
            )))
        }
    };
    let (items, schemas) = match statement.targtype() {
        GrantTargetType::AclTargetObject => (explicit_targets(statement, context)?, None),
        GrantTargetType::AclTargetAllInSchema => (
            Vec::new(),
            Some(
                statement
                    .objects
                    .iter()
                    .map(|object| {
                        let Some(NodeEnum::String(schema)) = object.node.as_ref() else {
                            return Err(SQLError::Internal(
                                "GRANT/REVOKE contains a malformed schema target".into(),
                            ));
                        };
                        Ok(schema.sval.clone())
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
        ),
        other => {
            return Err(SQLError::Unsupported(format!(
                "routine privileges do not support target type {other:?}"
            )))
        }
    };
    let privileges = statement
        .privileges
        .iter()
        .map(|node| {
            let Some(NodeEnum::AccessPriv(privilege)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed privilege".into(),
                ));
            };
            Ok(if !privilege.cols.is_empty() {
                RoutinePrivilege::ColumnsUnsupported
            } else if privilege.priv_name == "execute" {
                RoutinePrivilege::Execute
            } else {
                RoutinePrivilege::Unsupported(privilege.priv_name.clone())
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let grantees = statement
        .grantees
        .iter()
        .map(|grantee| {
            let Some(NodeEnum::RoleSpec(role)) = grantee.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed grantee".into(),
                ));
            };
            compile_acl_role_specification(role, "GRANT/REVOKE")
        })
        .collect::<Result<Vec<_>>>()?;
    let grantor = statement
        .grantor
        .as_ref()
        .map(|role| compile_role_specification(role, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantRoutine(GrantRoutineStmt {
        kind,
        is_grant: statement.is_grant,
        grant_option: statement.grant_option,
        grant_option_only: !statement.is_grant && statement.grant_option,
        items,
        schemas,
        privileges,
        grantees,
        grantor,
        revoke_behavior: if matches!(statement.behavior(), DropBehavior::DropCascade) {
            RoutineRevokeBehavior::Cascade
        } else {
            RoutineRevokeBehavior::Restrict
        },
    }))
}

fn explicit_targets(statement: &GrantStmt, context: &str) -> Result<Vec<GrantRoutineItem>> {
    statement
        .objects
        .iter()
        .map(|object| {
            let Some(NodeEnum::ObjectWithArgs(object)) = object.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed routine target".into(),
                ));
            };
            let CompiledRoutineTarget {
                name,
                arg_types,
                arg_type_references,
            } = compile_object_with_args(object, context)?;
            if !arg_type_references.is_empty() {
                return Err(SQLError::Unsupported(
                    "routine privilege targets using %TYPE are not supported".into(),
                ));
            }
            Ok(GrantRoutineItem { name, arg_types })
        })
        .collect()
}
