//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type declarations, descriptions, ownership, names and privileges share their object-kind lowering.

use pg_query::protobuf::{
    AlterObjectSchemaStmt, AlterOwnerStmt, DropBehavior, GrantStmt, GrantTargetType, Node,
    ObjectType, RenameStmt,
};

use super::routines::{compile_acl_role_specification, compile_role_specification};
use super::{domains::qualified_name, NodeEnum, Result, SQLError, Statement};
use crate::ast::{
    AlterTypeObject, AlterTypeObjectAction, GrantTypeStmt, TypeObjectKind, TypePrivilege,
    TypeRevokeBehavior,
};

/// The type object named by `TYPE` or `DOMAIN`; other object kinds keep their own lowering.
pub(super) fn type_object_kind(object_type: ObjectType) -> Option<TypeObjectKind> {
    match object_type {
        ObjectType::ObjectType => Some(TypeObjectKind::Type),
        ObjectType::ObjectDomain => Some(TypeObjectKind::Domain),
        _ => None,
    }
}

pub(super) fn compile_type_statement(node: &NodeEnum) -> Result<Statement> {
    match node {
        NodeEnum::CreateDomainStmt(stmt) => {
            super::domains::compile_create_domain(stmt).map(Statement::CreateDomain)
        }
        NodeEnum::AlterDomainStmt(stmt) => {
            super::domains::compile_alter_domain(stmt).map(Statement::AlterDomain)
        }
        NodeEnum::DefineStmt(stmt)
            if stmt.kind == ObjectType::ObjectType as i32
                && stmt.definition.is_empty()
                && stmt.args.is_empty() =>
        {
            qualified_name(&stmt.defnames).map(Statement::CreateShellType)
        }
        NodeEnum::CommentStmt(stmt) if type_object_kind(stmt.objtype()).is_some() => {
            compile_type_comment(stmt)
        }
        NodeEnum::CreateEnumStmt(stmt) => {
            super::enums::compile_create_enum(stmt).map(Statement::CreateEnum)
        }
        NodeEnum::CompositeTypeStmt(stmt) => super::composites::compile_create_composite_type(stmt)
            .map(Statement::CreateCompositeType),
        NodeEnum::AlterEnumStmt(stmt) => {
            super::enums::compile_alter_enum(stmt).map(Statement::AlterEnum)
        }
        other => Err(SQLError::Unsupported(
            super::dispatch::other_node_label(other).into(),
        )),
    }
}

fn compile_type_comment(stmt: &pg_query::protobuf::CommentStmt) -> Result<Statement> {
    let kind = type_object_kind(stmt.objtype())
        .ok_or_else(|| SQLError::Internal("type comment has another object kind".into()))?;
    let Some(NodeEnum::TypeName(name)) = stmt.object.as_deref().and_then(|node| node.node.as_ref())
    else {
        return Err(SQLError::Internal("type comment has no type name".into()));
    };
    let name = super::types::compile_pg_type_reference(name, "COMMENT ON TYPE")?.sql_name();
    Ok(Statement::CommentType(crate::ast::CommentType {
        kind,
        name,
        description: (!stmt.comment.is_empty()).then(|| stmt.comment.clone()),
    }))
}

fn type_name(object: Option<&Node>, context: &str) -> Result<String> {
    let Some(NodeEnum::List(list)) = object.and_then(|object| object.node.as_ref()) else {
        return Err(SQLError::Internal(format!("{context} has no type name")));
    };
    qualified_name(&list.items)
}

fn statement(kind: TypeObjectKind, name: String, action: AlterTypeObjectAction) -> Statement {
    Statement::AlterTypeObject(AlterTypeObject { kind, name, action })
}

pub(super) fn compile_type_rename(stmt: &RenameStmt, kind: TypeObjectKind) -> Result<Statement> {
    let name = type_name(stmt.object.as_deref(), "ALTER TYPE RENAME")?;
    Ok(statement(
        kind,
        name,
        AlterTypeObjectAction::RenameTo(stmt.newname.clone()),
    ))
}

pub(super) fn compile_type_set_schema(
    stmt: &AlterObjectSchemaStmt,
    kind: TypeObjectKind,
) -> Result<Statement> {
    let name = type_name(stmt.object.as_deref(), "ALTER TYPE SET SCHEMA")?;
    Ok(statement(
        kind,
        name,
        AlterTypeObjectAction::SetSchema(stmt.newschema.clone()),
    ))
}

pub(super) fn compile_type_owner(stmt: &AlterOwnerStmt, kind: TypeObjectKind) -> Result<Statement> {
    let name = type_name(stmt.object.as_deref(), "ALTER TYPE OWNER TO")?;
    let owner = stmt
        .newowner
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER TYPE OWNER TO has no owner".into()))?;
    Ok(statement(
        kind,
        name,
        AlterTypeObjectAction::OwnerTo(compile_role_specification(owner, "ALTER TYPE OWNER TO")?),
    ))
}

pub(super) fn compile_grant_type(stmt: &GrantStmt, kind: TypeObjectKind) -> Result<Statement> {
    if stmt.targtype() != GrantTargetType::AclTargetObject {
        return Err(SQLError::Internal(
            "type privileges have only object targets".into(),
        ));
    }
    // An empty privilege list is ALL [PRIVILEGES]; USAGE is the only type privilege.
    let mut privileges = Vec::with_capacity(stmt.privileges.len().max(1));
    for privilege in &stmt.privileges {
        let Some(NodeEnum::AccessPriv(privilege)) = privilege.node.as_ref() else {
            return Err(SQLError::Internal(
                "GRANT/REVOKE contains a malformed privilege".into(),
            ));
        };
        let privilege =
            if privilege.priv_name.eq_ignore_ascii_case("usage") && privilege.cols.is_empty() {
                TypePrivilege::Usage
            } else {
                TypePrivilege::Unsupported(privilege.priv_name.to_ascii_uppercase())
            };
        if !privileges.contains(&privilege) {
            privileges.push(privilege);
        }
    }
    if privileges.is_empty() {
        privileges.push(TypePrivilege::Usage);
    }
    let names = stmt
        .objects
        .iter()
        .map(|object| type_name(Some(object), "GRANT/REVOKE ON TYPE"))
        .collect::<Result<Vec<_>>>()?;
    let grantees = stmt
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
    let grantor = stmt
        .grantor
        .as_ref()
        .map(|role| compile_role_specification(role, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantType(GrantTypeStmt {
        kind,
        is_grant: stmt.is_grant,
        grant_option: stmt.grant_option,
        grant_option_only: !stmt.is_grant && stmt.grant_option,
        privileges,
        names,
        grantees,
        grantor,
        revoke_behavior: if matches!(stmt.behavior(), DropBehavior::DropCascade) {
            TypeRevokeBehavior::Cascade
        } else {
            TypeRevokeBehavior::Restrict
        },
    }))
}
