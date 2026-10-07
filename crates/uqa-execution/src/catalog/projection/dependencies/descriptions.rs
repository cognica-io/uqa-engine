//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `getObjectDescription`: an object named by its catalog, OID and column number, as `DROP` diagnostics and `pg_describe_object` name it. Relations, types and routines are named as their `reg*` output names them, qualified when the search path does not find them.

use super::objects::{CatalogObjects, ConstraintOwner, MemberObject, RelationKind};
use crate::catalog::context::CatalogContext;
use uqa_sql::ast::ColumnType;
use uqa_sql::catalog::dependencies::{
    ObjectAddress, ATTRIBUTE_DEFAULT_CLASS, CONSTRAINT_CLASS, DATABASE_CLASS, FOREIGN_SERVER_CLASS,
    FOREIGN_WRAPPER_CLASS, LANGUAGE_CLASS, NAMESPACE_CLASS, PROCEDURE_CLASS, RELATION_CLASS,
    REWRITE_CLASS, ROLE_CLASS, ROLE_MEMBERSHIP_CLASS, TRIGGER_CLASS, TYPE_CLASS,
};
use uqa_sql::catalog::languages::language_name;
use uqa_sql::SQLError;

/// Catalogs whose objects `getObjectDescription` can name and that hold no objects in this catalog.
const EMPTY_CLASSES: [u32; 27] = [
    826, 1213, 1418, 2601, 2602, 2603, 2605, 2607, 2613, 2616, 2617, 2753, 3079, 3256, 3381, 3456,
    3466, 3576, 3600, 3601, 3602, 3764, 6100, 6104, 6106, 6237, 6243,
];

pub(super) fn describe(
    context: &CatalogContext<'_>,
    objects: &CatalogObjects,
    object: ObjectAddress,
) -> Result<Option<String>, SQLError> {
    Ok(match object.class_id {
        RELATION_CLASS => {
            let Some(relation) = relation_description(context, objects, object.object_id)? else {
                return Ok(None);
            };
            if object.sub_id == 0 {
                Some(relation)
            } else {
                column_name(objects, object.object_id, object.sub_id)
                    .map(|name| format!("column {name} of {relation}"))
            }
        }
        PROCEDURE_CLASS => reg_output(context, &ColumnType::Regprocedure, object.object_id)?
            .map(|name| format!("function {name}")),
        TYPE_CLASS => {
            type_name(context, objects, object.object_id)?.map(|name| format!("type {name}"))
        }
        NAMESPACE_CLASS => objects
            .namespace_name(object.object_id)
            .map(|name| format!("schema {name}")),
        FOREIGN_SERVER_CLASS => objects
            .foreign_server_name(object.object_id)
            .map(|name| format!("server {name}")),
        FOREIGN_WRAPPER_CLASS => objects
            .foreign_wrapper_object(object.object_id)
            .map(|(name, _)| format!("foreign-data wrapper {name}")),
        ROLE_CLASS => objects
            .role_name(object.object_id)
            .map(|name| format!("role {name}")),
        DATABASE_CLASS => (i64::from(object.object_id) == uqa_sql::catalog::DATABASE_OID)
            .then(|| format!("database {}", uqa_sql::catalog::DATABASE_NAME)),
        LANGUAGE_CLASS => language_name(object.object_id).map(|name| format!("language {name}")),
        CONSTRAINT_CLASS
        | ATTRIBUTE_DEFAULT_CLASS
        | REWRITE_CLASS
        | TRIGGER_CLASS
        | ROLE_MEMBERSHIP_CLASS => {
            member_description(context, objects, object.class_id, object.object_id)?
        }
        class if EMPTY_CLASSES.contains(&class) => None,
        class => {
            return Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!("unsupported object class: {class}"),
            })
        }
    })
}

fn member_description(
    context: &CatalogContext<'_>,
    objects: &CatalogObjects,
    class_id: u32,
    oid: u32,
) -> Result<Option<String>, SQLError> {
    let Some(member) = objects.member(class_id, oid) else {
        return Ok(None);
    };
    let relation = |oid| relation_description(context, objects, oid);
    Ok(match member {
        MemberObject::Constraint {
            name,
            owner: ConstraintOwner::Domain(_),
            ..
        } => Some(format!("constraint {name}")),
        MemberObject::Constraint {
            name,
            owner: ConstraintOwner::Relation(oid),
            ..
        } => relation(*oid)?.map(|relation| format!("constraint {name} on {relation}")),
        MemberObject::AttributeDefault {
            relation: oid,
            column,
        } => {
            let Some(relation) = relation(*oid)? else {
                return Ok(None);
            };
            column_name(objects, *oid, *column)
                .map(|name| format!("default value for column {name} of {relation}"))
        }
        MemberObject::Rule {
            name,
            relation: oid,
        } => relation(*oid)?.map(|relation| format!("rule {name} on {relation}")),
        MemberObject::Trigger {
            name,
            relation: oid,
        } => relation(*oid)?.map(|relation| format!("trigger {name} on {relation}")),
        MemberObject::Membership { member, role } => objects
            .role_name(*member)
            .zip(objects.role_name(*role))
            .map(|(member, role)| format!("membership of role {member} in role {role}")),
    })
}

/// `getRelationDescription`: the relation's kind and name, qualified when the search path does not find it.
fn relation_description(
    context: &CatalogContext<'_>,
    objects: &CatalogObjects,
    oid: u32,
) -> Result<Option<String>, SQLError> {
    let kind = match objects.relation(oid) {
        Some(relation) => relation.kind,
        None => {
            let Some(relation) = system_relation(oid) else {
                return Ok(None);
            };
            if relation.kind() == "view" {
                RelationKind::View
            } else {
                RelationKind::Table
            }
        }
    };
    Ok(reg_output(context, &ColumnType::Regclass, oid)?
        .map(|name| format!("{} {name}", kind.description())))
}

/// The name of a relation's attribute; `None` for a column number the relation does not have.
fn column_name(objects: &CatalogObjects, relation: u32, column: i32) -> Option<String> {
    let index = usize::try_from(column).ok()?.checked_sub(1)?;
    match objects.relation(relation) {
        Some(relation) => relation
            .columns
            .iter()
            .enumerate()
            .find(|(index, candidate)| {
                uqa_sql::catalog::relation_attributes::column_number(candidate, *index)
                    .is_ok_and(|number| i32::from(number) == column)
            })
            .map(|(_, column)| column.name.clone())
            .or_else(|| {
                relation
                    .dropped_columns
                    .iter()
                    .find(|(number, _)| i32::from(*number) == column)
                    .map(|(_, name)| name.clone())
            }),
        None => system_relation(relation)?
            .column_names()
            .into_iter()
            .nth(index),
    }
}

fn system_relation(oid: u32) -> Option<uqa_sql::catalog::SystemRelation> {
    uqa_sql::catalog::SystemRelation::all().find(|relation| relation.oid() == i64::from(oid))
}

/// `format_type_be`: a relation's row type prints as its relation, and the row type's array type with brackets.
fn type_name(
    context: &CatalogContext<'_>,
    objects: &CatalogObjects,
    oid: u32,
) -> Result<Option<String>, SQLError> {
    let format = |oid: u32| {
        super::super::format_type_object(context, i64::from(oid)).map_err(SQLError::Internal)
    };
    if let Some(name) = format(oid)? {
        return Ok(Some(name));
    }
    match objects.row_type_of_array(oid) {
        Some(row_type) => Ok(format(row_type)?.map(|name| format!("{name}[]"))),
        None => Ok(None),
    }
}

fn reg_output(
    context: &CatalogContext<'_>,
    ty: &ColumnType,
    oid: u32,
) -> Result<Option<String>, SQLError> {
    super::super::resolve_regtype_output(context, ty, i64::from(oid)).map_err(SQLError::Internal)
}
