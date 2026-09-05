//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    compile_function_type_name, compile_qualified_name, def_elem_bool, NodeEnum, Result, SQLError,
    Statement,
};
use crate::compiler::range_var_name;

pub(in crate::compiler) fn compile_role_spec(
    role: &pg_query::protobuf::RoleSpec,
    allow_public: bool,
    context: &str,
) -> Result<String> {
    use pg_query::protobuf::RoleSpecType;
    match role.roletype() {
        RoleSpecType::RolespecCstring => Ok(role.rolename.clone()),
        RoleSpecType::RolespecCurrentRole | RoleSpecType::RolespecCurrentUser => {
            Ok("CURRENT_USER".into())
        }
        RoleSpecType::RolespecSessionUser => Ok("SESSION_USER".into()),
        RoleSpecType::RolespecPublic => Ok(if allow_public { "PUBLIC" } else { "public" }.into()),
        other => Err(SQLError::Unsupported(format!(
            "{context}: role specification {other:?} is not supported"
        ))),
    }
}

pub(in crate::compiler) struct CompiledRoutineTarget {
    pub(in crate::compiler) name: String,
    pub(in crate::compiler) arg_types: Option<Vec<String>>,
    pub(in crate::compiler) arg_type_references:
        Vec<Option<crate::ast::RoutineColumnTypeReference>>,
}

pub(in crate::compiler) fn compile_object_with_args(
    object: &pg_query::protobuf::ObjectWithArgs,
    context: &str,
) -> Result<CompiledRoutineTarget> {
    let name = compile_qualified_name(&object.objname, context)?;
    if object.args_unspecified {
        return Ok(CompiledRoutineTarget {
            name,
            arg_types: None,
            arg_type_references: Vec::new(),
        });
    }
    let mut types = Vec::with_capacity(object.objargs.len());
    let mut references = Vec::with_capacity(object.objargs.len());
    for argument in &object.objargs {
        let Some(NodeEnum::TypeName(type_name)) = argument.node.as_ref() else {
            return Err(SQLError::Unsupported(format!(
                "{context}: malformed argument type"
            )));
        };
        let compiled = compile_function_type_name(type_name)?;
        types.push(compiled.name);
        references.push(compiled.reference);
    }
    if references.iter().all(Option::is_none) {
        references.clear();
    }
    Ok(CompiledRoutineTarget {
        name,
        arg_types: Some(types),
        arg_type_references: references,
    })
}

pub(in crate::compiler) fn compile_alter_routine_owner(
    statement: &pg_query::protobuf::AlterOwnerStmt,
) -> Result<Statement> {
    use crate::ast::{AlterRoutineKind, AlterRoutineOwnerStmt};
    use pg_query::protobuf::ObjectType;
    let (kind, context) = match statement.object_type() {
        ObjectType::ObjectFunction => (AlterRoutineKind::Function, "ALTER FUNCTION"),
        ObjectType::ObjectProcedure => (AlterRoutineKind::Procedure, "ALTER PROCEDURE"),
        ObjectType::ObjectRoutine => (AlterRoutineKind::Routine, "ALTER ROUTINE"),
        other => {
            return Err(SQLError::Unsupported(format!(
                "ALTER OWNER target {other:?} is not supported"
            )))
        }
    };
    let Some(NodeEnum::ObjectWithArgs(object)) = statement
        .object
        .as_deref()
        .and_then(|object| object.node.as_ref())
    else {
        return Err(SQLError::Internal(format!(
            "{context}: malformed routine target"
        )));
    };
    let CompiledRoutineTarget {
        name,
        arg_types,
        arg_type_references,
    } = compile_object_with_args(object, context)?;
    let owner = statement
        .newowner
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{context}: owner is missing")))?;
    Ok(Statement::AlterRoutineOwner(AlterRoutineOwnerStmt {
        kind,
        name,
        arg_types,
        arg_type_references,
        new_owner: compile_role_spec(owner, false, context)?,
    }))
}

pub(in crate::compiler) fn compile_grant(
    statement: &pg_query::protobuf::GrantStmt,
) -> Result<Statement> {
    use pg_query::protobuf::ObjectType;
    match statement.objtype() {
        ObjectType::ObjectTable => compile_grant_table(statement),
        ObjectType::ObjectSequence => compile_grant_sequence(statement),
        ObjectType::ObjectDatabase => compile_grant_database(statement),
        ObjectType::ObjectSchema => compile_grant_schema(statement),
        ObjectType::ObjectFunction | ObjectType::ObjectProcedure | ObjectType::ObjectRoutine => {
            compile_grant_routine(statement)
        }
        other => Err(SQLError::Unsupported(format!(
            "GRANT/REVOKE object type {other:?} is not supported"
        ))),
    }
}

fn compile_grant_table(statement: &pg_query::protobuf::GrantStmt) -> Result<Statement> {
    use crate::ast::{GrantTableStmt, TableRevokeBehavior};
    use pg_query::protobuf::DropBehavior;

    let target = compile_grant_table_target(statement)?;
    let privileges = compile_grant_table_privileges(statement)?;
    let grantees = statement
        .grantees
        .iter()
        .map(|grantee| {
            let Some(NodeEnum::RoleSpec(role)) = grantee.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed grantee".into(),
                ));
            };
            compile_role_spec(role, true, "GRANT/REVOKE")
        })
        .collect::<Result<Vec<_>>>()?;
    let grantor = statement
        .grantor
        .as_ref()
        .map(|role| compile_role_spec(role, false, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantTable(GrantTableStmt {
        is_grant: statement.is_grant,
        grant_option: statement.grant_option,
        grant_option_only: !statement.is_grant && statement.grant_option,
        privileges,
        target,
        grantees,
        grantor,
        revoke_behavior: if matches!(statement.behavior(), DropBehavior::DropCascade) {
            TableRevokeBehavior::Cascade
        } else {
            TableRevokeBehavior::Restrict
        },
    }))
}

fn compile_grant_table_target(
    statement: &pg_query::protobuf::GrantStmt,
) -> Result<crate::ast::GrantTableTarget> {
    use crate::ast::GrantTableTarget;
    use pg_query::protobuf::GrantTargetType;

    match statement.targtype() {
        GrantTargetType::AclTargetObject => {
            let mut names = Vec::with_capacity(statement.objects.len());
            for object in &statement.objects {
                let Some(NodeEnum::RangeVar(relation)) = object.node.as_ref() else {
                    return Err(SQLError::Internal(
                        "GRANT/REVOKE contains a malformed table target".into(),
                    ));
                };
                names.push(range_var_name(relation));
            }
            Ok(GrantTableTarget::Relations { names })
        }
        GrantTargetType::AclTargetAllInSchema => {
            let schemas = statement
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
                .collect::<Result<Vec<_>>>()?;
            Ok(GrantTableTarget::AllTablesInSchemas { schemas })
        }
        other => Err(SQLError::Unsupported(format!(
            "table privileges do not support target type {other:?}"
        ))),
    }
}

fn compile_grant_table_privileges(
    statement: &pg_query::protobuf::GrantStmt,
) -> Result<Vec<crate::ast::TablePrivilegeSpec>> {
    use crate::ast::{TablePrivilege, TablePrivilegeSpec};

    let mut privileges = Vec::with_capacity(statement.privileges.len());
    for privilege in &statement.privileges {
        let Some(NodeEnum::AccessPriv(privilege)) = privilege.node.as_ref() else {
            return Err(SQLError::Internal(
                "GRANT/REVOKE contains a malformed privilege".into(),
            ));
        };
        let privilege_name = match privilege.priv_name.to_ascii_lowercase().as_str() {
            "select" => TablePrivilege::Select,
            "insert" => TablePrivilege::Insert,
            "update" => TablePrivilege::Update,
            "delete" => TablePrivilege::Delete,
            "truncate" => TablePrivilege::Truncate,
            "references" => TablePrivilege::References,
            "trigger" => TablePrivilege::Trigger,
            "maintain" => TablePrivilege::Maintain,
            "usage" => TablePrivilege::Usage,
            _ => TablePrivilege::Unsupported(privilege.priv_name.to_ascii_uppercase()),
        };
        let columns = privilege
            .cols
            .iter()
            .map(|column| {
                let Some(NodeEnum::String(column)) = column.node.as_ref() else {
                    return Err(SQLError::Internal(
                        "GRANT/REVOKE contains a malformed column privilege".into(),
                    ));
                };
                Ok(column.sval.clone())
            })
            .collect::<Result<Vec<_>>>()?;
        let spec = TablePrivilegeSpec {
            privilege: privilege_name,
            columns,
        };
        if !privileges.contains(&spec) {
            privileges.push(spec);
        }
    }
    Ok(privileges)
}

fn compile_grant_database(statement: &pg_query::protobuf::GrantStmt) -> Result<Statement> {
    use crate::ast::{DatabasePrivilege, DatabaseRevokeBehavior, GrantDatabaseStmt};
    use pg_query::protobuf::{DropBehavior, GrantTargetType};

    if statement.targtype() != GrantTargetType::AclTargetObject {
        return Err(SQLError::Unsupported(
            "database privileges require explicit database targets".into(),
        ));
    }
    let privileges = if statement.privileges.is_empty() {
        vec![
            DatabasePrivilege::Connect,
            DatabasePrivilege::Create,
            DatabasePrivilege::Temporary,
        ]
    } else {
        let mut privileges = Vec::with_capacity(statement.privileges.len());
        for privilege in &statement.privileges {
            let Some(NodeEnum::AccessPriv(privilege)) = privilege.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed privilege".into(),
                ));
            };
            let privilege = match privilege.priv_name.to_ascii_lowercase().as_str() {
                "connect" if privilege.cols.is_empty() => DatabasePrivilege::Connect,
                "create" if privilege.cols.is_empty() => DatabasePrivilege::Create,
                "temporary" | "temp" if privilege.cols.is_empty() => DatabasePrivilege::Temporary,
                _ => DatabasePrivilege::Unsupported(privilege.priv_name.to_ascii_uppercase()),
            };
            if !privileges.contains(&privilege) {
                privileges.push(privilege);
            }
        }
        privileges
    };
    let databases = statement
        .objects
        .iter()
        .map(|object| {
            let Some(NodeEnum::String(database)) = object.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed database target".into(),
                ));
            };
            Ok(database.sval.clone())
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
            compile_role_spec(role, true, "GRANT/REVOKE")
        })
        .collect::<Result<Vec<_>>>()?;
    let grantor = statement
        .grantor
        .as_ref()
        .map(|role| compile_role_spec(role, false, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantDatabase(GrantDatabaseStmt {
        is_grant: statement.is_grant,
        grant_option: statement.grant_option,
        grant_option_only: !statement.is_grant && statement.grant_option,
        privileges,
        databases,
        grantees,
        grantor,
        revoke_behavior: if matches!(statement.behavior(), DropBehavior::DropCascade) {
            DatabaseRevokeBehavior::Cascade
        } else {
            DatabaseRevokeBehavior::Restrict
        },
    }))
}

fn compile_grant_schema(statement: &pg_query::protobuf::GrantStmt) -> Result<Statement> {
    use crate::ast::{GrantSchemaStmt, SchemaPrivilege, SchemaRevokeBehavior};
    use pg_query::protobuf::{DropBehavior, GrantTargetType};

    if statement.targtype() != GrantTargetType::AclTargetObject {
        return Err(SQLError::Unsupported(
            "schema privileges require explicit schema targets".into(),
        ));
    }
    let privileges = if statement.privileges.is_empty() {
        vec![SchemaPrivilege::Usage, SchemaPrivilege::Create]
    } else {
        let mut privileges = Vec::with_capacity(statement.privileges.len());
        for privilege in &statement.privileges {
            let Some(NodeEnum::AccessPriv(privilege)) = privilege.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed privilege".into(),
                ));
            };
            let privilege = match privilege.priv_name.to_ascii_lowercase().as_str() {
                "usage" if privilege.cols.is_empty() => SchemaPrivilege::Usage,
                "create" if privilege.cols.is_empty() => SchemaPrivilege::Create,
                _ => SchemaPrivilege::Unsupported(privilege.priv_name.to_ascii_uppercase()),
            };
            if !privileges.contains(&privilege) {
                privileges.push(privilege);
            }
        }
        privileges
    };
    let schemas = statement
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
            compile_role_spec(role, true, "GRANT/REVOKE")
        })
        .collect::<Result<Vec<_>>>()?;
    let grantor = statement
        .grantor
        .as_ref()
        .map(|role| compile_role_spec(role, false, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantSchema(GrantSchemaStmt {
        is_grant: statement.is_grant,
        grant_option: statement.grant_option,
        grant_option_only: !statement.is_grant && statement.grant_option,
        privileges,
        schemas,
        grantees,
        grantor,
        revoke_behavior: if matches!(statement.behavior(), DropBehavior::DropCascade) {
            SchemaRevokeBehavior::Cascade
        } else {
            SchemaRevokeBehavior::Restrict
        },
    }))
}

fn compile_grant_routine(statement: &pg_query::protobuf::GrantStmt) -> Result<Statement> {
    use crate::ast::{AlterRoutineKind, GrantRoutineItem, GrantRoutineStmt, RoutineRevokeBehavior};
    use pg_query::protobuf::{DropBehavior, GrantTargetType, ObjectType};
    if statement.targtype() != GrantTargetType::AclTargetObject {
        return Err(SQLError::Unsupported(
            "routine privileges require explicit object targets".into(),
        ));
    }
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
    // PostgreSQL represents ALL [PRIVILEGES] with an empty privilege list.
    // EXECUTE is the sole routine privilege, so it is equivalent here.
    for privilege in &statement.privileges {
        let Some(NodeEnum::AccessPriv(privilege)) = privilege.node.as_ref() else {
            return Err(SQLError::Internal(
                "GRANT/REVOKE contains a malformed privilege".into(),
            ));
        };
        if !privilege.priv_name.eq_ignore_ascii_case("execute") || !privilege.cols.is_empty() {
            return Err(SQLError::Unsupported(format!(
                "only EXECUTE is valid for {context} privileges"
            )));
        }
    }
    let mut items = Vec::with_capacity(statement.objects.len());
    for object in &statement.objects {
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
        items.push(GrantRoutineItem { name, arg_types });
    }
    let grantees = statement
        .grantees
        .iter()
        .map(|grantee| {
            let Some(NodeEnum::RoleSpec(role)) = grantee.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed grantee".into(),
                ));
            };
            compile_role_spec(role, true, "GRANT/REVOKE")
        })
        .collect::<Result<Vec<_>>>()?;
    let grantor = statement
        .grantor
        .as_ref()
        .map(|role| compile_role_spec(role, false, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantRoutine(GrantRoutineStmt {
        kind,
        is_grant: statement.is_grant,
        grant_option: statement.grant_option,
        grant_option_only: !statement.is_grant && statement.grant_option,
        items,
        grantees,
        grantor,
        revoke_behavior: if matches!(statement.behavior(), DropBehavior::DropCascade) {
            RoutineRevokeBehavior::Cascade
        } else {
            RoutineRevokeBehavior::Restrict
        },
    }))
}

fn compile_grant_sequence(statement: &pg_query::protobuf::GrantStmt) -> Result<Statement> {
    use crate::ast::{GrantSequenceStmt, SequencePrivilege, SequenceRevokeBehavior};
    use pg_query::protobuf::DropBehavior;

    let target = compile_grant_sequence_target(statement)?;
    let privileges = if statement.privileges.is_empty() {
        vec![
            SequencePrivilege::Select,
            SequencePrivilege::Update,
            SequencePrivilege::Usage,
        ]
    } else {
        let mut privileges = Vec::with_capacity(statement.privileges.len());
        for privilege in &statement.privileges {
            let Some(NodeEnum::AccessPriv(privilege)) = privilege.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed privilege".into(),
                ));
            };
            let privilege = if privilege.cols.is_empty() {
                match privilege.priv_name.to_ascii_lowercase().as_str() {
                    "select" => SequencePrivilege::Select,
                    "update" => SequencePrivilege::Update,
                    "usage" => SequencePrivilege::Usage,
                    _ => SequencePrivilege::Unsupported(privilege.priv_name.to_ascii_uppercase()),
                }
            } else {
                SequencePrivilege::ColumnsUnsupported
            };
            if !privileges.contains(&privilege) {
                privileges.push(privilege);
            }
        }
        privileges
    };
    let grantees = statement
        .grantees
        .iter()
        .map(|grantee| {
            let Some(NodeEnum::RoleSpec(role)) = grantee.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE contains a malformed grantee".into(),
                ));
            };
            compile_role_spec(role, true, "GRANT/REVOKE")
        })
        .collect::<Result<Vec<_>>>()?;
    let grantor = statement
        .grantor
        .as_ref()
        .map(|role| compile_role_spec(role, false, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantSequence(GrantSequenceStmt {
        is_grant: statement.is_grant,
        grant_option: statement.grant_option,
        grant_option_only: !statement.is_grant && statement.grant_option,
        privileges,
        target,
        grantees,
        grantor,
        revoke_behavior: if matches!(statement.behavior(), DropBehavior::DropCascade) {
            SequenceRevokeBehavior::Cascade
        } else {
            SequenceRevokeBehavior::Restrict
        },
    }))
}

fn compile_grant_sequence_target(
    statement: &pg_query::protobuf::GrantStmt,
) -> Result<crate::ast::GrantSequenceTarget> {
    use crate::ast::GrantSequenceTarget;
    use pg_query::protobuf::{GrantTargetType, ObjectType};

    let object_type = statement.objtype();
    match statement.targtype() {
        GrantTargetType::AclTargetObject => {
            let mut names = Vec::with_capacity(statement.objects.len());
            for object in &statement.objects {
                let Some(NodeEnum::RangeVar(relation)) = object.node.as_ref() else {
                    return Err(SQLError::Internal(
                        "GRANT/REVOKE contains a malformed sequence target".into(),
                    ));
                };
                names.push(range_var_name(relation));
            }
            Ok(GrantSequenceTarget::Sequences { names })
        }
        GrantTargetType::AclTargetAllInSchema if object_type == ObjectType::ObjectSequence => {
            let schemas = statement
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
                .collect::<Result<Vec<_>>>()?;
            Ok(GrantSequenceTarget::AllSequencesInSchemas { schemas })
        }
        other => Err(SQLError::Unsupported(format!(
            "sequence privileges do not support target type {other:?}"
        ))),
    }
}

fn compile_membership_role_list(
    nodes: &[pg_query::protobuf::Node],
    context: &str,
) -> Result<Vec<String>> {
    nodes
        .iter()
        .map(|node| {
            let Some(NodeEnum::RoleSpec(role)) = node.node.as_ref() else {
                return Err(SQLError::Internal(format!(
                    "{context} contains a malformed role specification"
                )));
            };
            compile_role_spec(role, false, context)
        })
        .collect()
}

pub(in crate::compiler) fn compile_grant_role(
    statement: &pg_query::protobuf::GrantRoleStmt,
) -> Result<Statement> {
    use crate::ast::{GrantRoleStmt, RoleMembershipOptions};
    use pg_query::protobuf::DropBehavior;

    let granted_roles = statement
        .granted_roles
        .iter()
        .map(|node| {
            let Some(NodeEnum::AccessPriv(role)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "GRANT/REVOKE ROLE contains a malformed granted role".into(),
                ));
            };
            if !role.cols.is_empty() {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "column lists are not valid for role membership".into(),
                });
            }
            Ok(role.priv_name.clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let members = compile_membership_role_list(&statement.grantee_roles, "GRANT/REVOKE ROLE")?;
    let mut options = RoleMembershipOptions::default();
    for option in &statement.opt {
        let Some(NodeEnum::DefElem(option)) = option.node.as_ref() else {
            return Err(SQLError::Internal(
                "GRANT/REVOKE ROLE contains a malformed option".into(),
            ));
        };
        let slot = match option.defname.to_ascii_lowercase().as_str() {
            "admin" => &mut options.admin,
            "inherit" => &mut options.inherit,
            "set" => &mut options.set,
            other => {
                return Err(SQLError::Unsupported(format!(
                    "GRANT/REVOKE ROLE option `{other}` is not supported"
                )))
            }
        };
        if slot.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!(
                    "conflicting or redundant role membership option `{}`",
                    option.defname
                ),
            });
        }
        *slot = Some(def_elem_bool(option, "GRANT/REVOKE ROLE option")?);
    }
    let grantor = statement
        .grantor
        .as_ref()
        .map(|role| compile_role_spec(role, false, "GRANTED BY"))
        .transpose()?;
    Ok(Statement::GrantRole(GrantRoleStmt {
        granted_roles,
        grantee_roles: members,
        is_grant: statement.is_grant,
        options,
        grantor,
        cascade: matches!(statement.behavior(), DropBehavior::DropCascade),
    }))
}

fn role_option_bool(element: &pg_query::protobuf::DefElem, context: &str) -> Result<bool> {
    def_elem_bool(element, context)
}

fn role_option_i32(element: &pg_query::protobuf::DefElem, context: &str) -> Result<i32> {
    match element
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    {
        Some(NodeEnum::Integer(value)) => Ok(value.ival),
        other => Err(SQLError::TypeMismatch(format!(
            "{context} expects an integer, got {other:?}"
        ))),
    }
}

pub(in crate::compiler) fn compile_create_role(
    statement: &pg_query::protobuf::CreateRoleStmt,
) -> Result<Statement> {
    use crate::ast::{CreateRoleStmt, RoleAttribute};
    use pg_query::protobuf::RoleStmtType;
    let mut attributes = std::collections::BTreeSet::from([RoleAttribute::Inherit]);
    if statement.stmt_type() == RoleStmtType::RolestmtUser {
        attributes.insert(RoleAttribute::Login);
    }
    let mut role = CreateRoleStmt {
        name: statement.role.clone(),
        attributes,
        connection_limit: -1,
        in_roles: Vec::new(),
        role_members: Vec::new(),
        admin_members: Vec::new(),
    };
    for option in &statement.options {
        let Some(NodeEnum::DefElem(element)) = option.node.as_ref() else {
            return Err(SQLError::Internal(
                "CREATE ROLE has a malformed option".into(),
            ));
        };
        let (attribute, context) = match element.defname.to_ascii_lowercase().as_str() {
            "superuser" => (RoleAttribute::Superuser, "SUPERUSER"),
            "inherit" => (RoleAttribute::Inherit, "INHERIT"),
            "createrole" => (RoleAttribute::CreateRole, "CREATEROLE"),
            "createdb" => (RoleAttribute::CreateDb, "CREATEDB"),
            "canlogin" => (RoleAttribute::Login, "LOGIN"),
            "isreplication" => (RoleAttribute::Replication, "REPLICATION"),
            "bypassrls" => (RoleAttribute::BypassRls, "BYPASSRLS"),
            "connectionlimit" => {
                role.connection_limit = role_option_i32(element, "CONNECTION LIMIT")?;
                continue;
            }
            "addroleto" | "rolemembers" | "adminmembers" => {
                let Some(NodeEnum::List(roles)) =
                    element.arg.as_ref().and_then(|node| node.node.as_ref())
                else {
                    return Err(SQLError::Internal(format!(
                        "CREATE ROLE option `{}` has a malformed role list",
                        element.defname
                    )));
                };
                let names = compile_membership_role_list(&roles.items, "CREATE ROLE membership")?;
                match element.defname.as_str() {
                    "addroleto" => role.in_roles.extend(names),
                    "rolemembers" => role.role_members.extend(names),
                    "adminmembers" => role.admin_members.extend(names),
                    _ => unreachable!(),
                }
                continue;
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE ROLE option `{other}` is not supported"
                )))
            }
        };
        if role_option_bool(element, context)? {
            role.attributes.insert(attribute);
        } else {
            role.attributes.remove(&attribute);
        }
    }
    Ok(Statement::CreateRole(role))
}

pub(in crate::compiler) fn compile_alter_role(
    statement: &pg_query::protobuf::AlterRoleStmt,
) -> Result<Statement> {
    use crate::ast::{AlterRoleStmt, RoleAttribute};
    let role = statement
        .role
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER ROLE has no target".into()))?;
    let mut alter = AlterRoleStmt {
        name: compile_role_spec(role, false, "ALTER ROLE")?,
        attributes: std::collections::BTreeMap::new(),
        connection_limit: None,
        membership_action: None,
        members: Vec::new(),
    };
    let membership_option = statement.options.iter().find_map(|option| {
        let NodeEnum::DefElem(element) = option.node.as_ref()? else {
            return None;
        };
        (element.defname == "rolemembers").then_some(element)
    });
    if let Some(element) = membership_option {
        use crate::ast::RoleMembershipAction;
        let Some(NodeEnum::List(roles)) = element.arg.as_ref().and_then(|node| node.node.as_ref())
        else {
            return Err(SQLError::Internal(
                "ALTER GROUP has a malformed member list".into(),
            ));
        };
        alter.membership_action = Some(if statement.action > 0 {
            RoleMembershipAction::Add
        } else {
            RoleMembershipAction::Drop
        });
        alter.members = compile_membership_role_list(&roles.items, "ALTER GROUP")?;
        return Ok(Statement::AlterRole(alter));
    }
    for option in &statement.options {
        let Some(NodeEnum::DefElem(element)) = option.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER ROLE has a malformed option".into(),
            ));
        };
        let (attribute, context) = match element.defname.to_ascii_lowercase().as_str() {
            "superuser" => (RoleAttribute::Superuser, "SUPERUSER"),
            "inherit" => (RoleAttribute::Inherit, "INHERIT"),
            "createrole" => (RoleAttribute::CreateRole, "CREATEROLE"),
            "createdb" => (RoleAttribute::CreateDb, "CREATEDB"),
            "canlogin" => (RoleAttribute::Login, "LOGIN"),
            "isreplication" => (RoleAttribute::Replication, "REPLICATION"),
            "bypassrls" => (RoleAttribute::BypassRls, "BYPASSRLS"),
            "connectionlimit" => {
                alter.connection_limit = Some(role_option_i32(element, "CONNECTION LIMIT")?);
                continue;
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER ROLE option `{other}` is not supported"
                )))
            }
        };
        alter
            .attributes
            .insert(attribute, role_option_bool(element, context)?);
    }
    Ok(Statement::AlterRole(alter))
}

pub(in crate::compiler) fn compile_drop_role(
    statement: &pg_query::protobuf::DropRoleStmt,
) -> Result<Statement> {
    use crate::ast::DropRoleStmt;
    let names = statement
        .roles
        .iter()
        .map(|role| {
            let Some(NodeEnum::RoleSpec(role)) = role.node.as_ref() else {
                return Err(SQLError::Internal(
                    "DROP ROLE has a malformed target".into(),
                ));
            };
            compile_role_spec(role, false, "DROP ROLE")
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Statement::DropRole(DropRoleStmt {
        names,
        if_exists: statement.missing_ok,
    }))
}
