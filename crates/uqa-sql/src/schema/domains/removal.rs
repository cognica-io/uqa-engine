//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP DOMAIN, DROP TYPE and ALTER TYPE name binding, namespace privileges, owner and required-type diagnostics.

use crate::catalog::roles::RoleReference;
use crate::{
    catalog::{domain::DomainCatalog, roles::RoleReferenceNames, security::BoundSchemaSecurity},
    SQLError,
};

pub trait TypeObjectCatalog: DomainCatalog {
    fn schema_security(&self, name: &str) -> Option<BoundSchemaSecurity>;
    fn resolve_drop_type_oid(&self, name: &str) -> Result<Option<i64>, SQLError>;
    fn format_drop_type(&self, oid: i64) -> Result<Option<String>, String>;
    /// An enum by its own type OID or by its generated array's OID.
    fn enum_by_type_oid(&self, oid: u32) -> Option<crate::catalog::enum_type::StoredEnum>;
    /// A standalone composite type by its own type OID or by its generated array's OID.
    fn composite_by_type_oid(
        &self,
        oid: u32,
    ) -> Option<crate::catalog::composite_type::StoredComposite>;
    /// The element type of a generated array of a user-defined type.
    fn user_array_element(&self, oid: u32) -> Option<u32>;
    /// The relation whose row type has this OID: its object kind, display name and owner.
    fn row_type_relation(&self, oid: u32) -> Option<RowTypeRelation>;
}

/// A relation that owns a composite row type, described for dependency diagnostics.
#[derive(Debug, Clone)]
pub struct RowTypeRelation {
    pub kind: &'static str,
    /// The relation named as `regclass` output spells it.
    pub name: String,
    /// The relation's unqualified catalog name.
    pub local_name: String,
    pub owner: uqa_core::catalog_role::RoleIdentity,
    pub schema: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TypeDropKind {
    Domain,
    Type,
}
pub trait TypeObjectAuthority {
    fn schema_usage(&self, schema: &str, role: &RoleReference) -> bool;
    fn current_user_has_role_privileges(
        &self,
        role: &dyn crate::catalog::roles::identity::RoleSubject,
    ) -> bool;
}
pub struct TypeObjectBinding<'a> {
    pub catalog: &'a dyn TypeObjectCatalog,
    pub authority: &'a dyn TypeObjectAuthority,
    pub session: &'a dyn RoleReferenceNames,
}
pub enum BoundTypeDrop {
    Target(u32),
    Skipped(String),
}

pub fn resolve_drop_domain(
    context: &TypeObjectBinding<'_>,
    name: &str,
    if_exists: bool,
) -> Result<BoundTypeDrop, SQLError> {
    resolve_type_drop(context, name, if_exists, TypeDropKind::Domain)
}

/// `DROP TYPE` removes domains, enums and composite types. Built-in, generated array and relation row types are required by other objects and fail after the owner check, as in `PostgreSQL`.
pub fn resolve_drop_type(
    context: &TypeObjectBinding<'_>,
    name: &str,
    if_exists: bool,
) -> Result<BoundTypeDrop, SQLError> {
    resolve_type_drop(context, name, if_exists, TypeDropKind::Type)
}

fn resolve_type_drop(
    context: &TypeObjectBinding<'_>,
    name: &str,
    if_exists: bool,
    kind: TypeDropKind,
) -> Result<BoundTypeDrop, SQLError> {
    let parsed = crate::parse_regtype_name(name)?
        .ok_or_else(|| SQLError::Internal("DROP has no type name".into()))?;
    let label = format!(
        "{}{}",
        parsed.names.join("."),
        "[]".repeat(parsed.array_dimensions)
    );
    if let [schema, _] = parsed.names.as_slice() {
        if context.catalog.schema_security(schema).is_none() {
            if if_exists {
                return Ok(BoundTypeDrop::Skipped(format!(
                    "schema \"{schema}\" does not exist, skipping"
                )));
            }
            return Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            });
        }
        if !context
            .authority
            .schema_usage(schema, &context.session.current_role())
        {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("permission denied for schema {schema}"),
            });
        }
    }
    let oid = context.catalog.resolve_drop_type_oid(name)?;
    let Some(oid) = oid else {
        if if_exists {
            return Ok(BoundTypeDrop::Skipped(format!(
                "type \"{label}\" does not exist, skipping"
            )));
        }
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{label}\" does not exist"),
        });
    };
    let type_oid = u32::try_from(oid)
        .map_err(|_| SQLError::Internal(format!("DROP type OID {oid} is out of range")))?;
    let target = drop_target(context, type_oid, &label, kind)?;
    let owns_schema = target.schema.as_ref().is_some_and(|schema| {
        context
            .catalog
            .schema_security(schema)
            .is_some_and(|security| {
                context
                    .authority
                    .current_user_has_role_privileges(&security.role_owner)
            })
    });
    if !owns_schema
        && !context
            .authority
            .current_user_has_role_privileges(&target.owner)
    {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of type {}", format_type(context, oid)?),
        });
    }
    match target.required_by {
        None => Ok(BoundTypeDrop::Target(type_oid)),
        Some(Requirement::System) => Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop type {} because it is required by the database system",
                format_type(context, oid)?
            ),
        }),
        Some(Requirement::Object(object)) => Err(SQLError::Diagnostic {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop type {} because {object} requires it",
                format_type(context, oid)?
            ),
            detail: None,
            hint: Some(format!("You can drop {object} instead.")),
        }),
    }
}

enum Requirement {
    System,
    Object(String),
}

struct DropTarget {
    schema: Option<String>,
    owner: uqa_core::catalog_role::RoleIdentity,
    required_by: Option<Requirement>,
}

fn drop_target(
    context: &TypeObjectBinding<'_>,
    oid: u32,
    label: &str,
    kind: TypeDropKind,
) -> Result<DropTarget, SQLError> {
    if let Some(domain) = context.catalog.domain_by_oid(oid) {
        return Ok(DropTarget {
            schema: Some(domain.identity.schema),
            owner: domain.owner,
            required_by: None,
        });
    }
    if kind == TypeDropKind::Domain {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{label}\" is not a domain"),
        });
    }
    if let Some(definition) = context.catalog.enum_by_type_oid(oid) {
        return Ok(DropTarget {
            schema: Some(definition.identity.schema.clone()),
            owner: definition.owner,
            required_by: (definition.array_oid == oid)
                .then(|| {
                    format_type(context, i64::from(definition.oid))
                        .map(|element| Requirement::Object(format!("type {element}")))
                })
                .transpose()?,
        });
    }
    if let Some(definition) = context.catalog.composite_by_type_oid(oid) {
        return Ok(DropTarget {
            schema: Some(definition.identity.schema.clone()),
            owner: definition.owner,
            required_by: (definition.array_oid == oid)
                .then(|| {
                    format_type(context, i64::from(definition.oid))
                        .map(|element| Requirement::Object(format!("type {element}")))
                })
                .transpose()?,
        });
    }
    if let Some(element) = context.catalog.user_array_element(oid) {
        let domain = context
            .catalog
            .domain_by_oid(element)
            .ok_or_else(|| SQLError::Internal(format!("array type {oid} has no element type")))?;
        return Ok(DropTarget {
            schema: Some(domain.identity.schema),
            owner: domain.owner,
            required_by: Some(Requirement::Object(format!(
                "type {}",
                format_type(context, i64::from(element))?
            ))),
        });
    }
    if let Some(relation) = context.catalog.row_type_relation(oid) {
        return Ok(DropTarget {
            schema: Some(relation.schema),
            owner: relation.owner,
            required_by: Some(Requirement::Object(format!(
                "{} {}",
                relation.kind, relation.name
            ))),
        });
    }
    Ok(DropTarget {
        schema: None,
        owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        required_by: Some(Requirement::System),
    })
}

fn format_type(context: &TypeObjectBinding<'_>, oid: i64) -> Result<String, SQLError> {
    context
        .catalog
        .format_drop_type(oid)
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::Internal("DROP type target disappeared".into()))
}

/// Resolve the enum named by `ALTER TYPE ... ADD VALUE | RENAME VALUE` with `PostgreSQL`'s `checkEnumOwner` order: missing type, then a non-enum type, then ownership.
pub fn resolve_alter_enum(
    context: &TypeObjectBinding<'_>,
    name: &str,
) -> Result<crate::catalog::enum_type::StoredEnum, SQLError> {
    let parsed = crate::parse_regtype_name(name)?
        .ok_or_else(|| SQLError::Internal("ALTER TYPE has no type name".into()))?;
    let label = format!(
        "{}{}",
        parsed.names.join("."),
        "[]".repeat(parsed.array_dimensions)
    );
    let oid = context
        .catalog
        .resolve_drop_type_oid(name)?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{label}\" does not exist"),
        })?;
    let definition = u32::try_from(oid)
        .ok()
        .and_then(|oid| context.catalog.enum_by_type_oid(oid))
        .filter(|definition| i64::from(definition.oid) == oid)
        .ok_or_else(|| {
            format_type(context, oid).map_or_else(
                |error| error,
                |name| SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("{name} is not an enum"),
                },
            )
        })?;
    // Unlike DROP, altering an enum requires ownership of the type itself.
    if !context
        .authority
        .current_user_has_role_privileges(&definition.owner)
    {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of type {}", format_type(context, oid)?),
        });
    }
    Ok(definition)
}
