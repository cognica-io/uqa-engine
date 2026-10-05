//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Targets of `ALTER TYPE | DOMAIN` and `GRANT | REVOKE ... ON TYPE | DOMAIN`: the user-defined type a name resolves to, and `PostgreSQL`'s diagnostics for the types a command cannot change. Each command applies the checks in its own `PostgreSQL` order.

use crate::ast::{ObjectAclEntry, TypeObjectKind};
use crate::catalog::{
    composite_type::StoredComposite, domain::StoredDomain, enum_type::StoredEnum,
};
use crate::schema::domains::removal::{RowTypeRelation, TypeObjectBinding};
use crate::SQLError;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

/// A user-defined type whose catalog definition a lifecycle command changes. Its generated array type follows it.
#[derive(Debug, Clone)]
pub enum TypeObject {
    Enum(StoredEnum),
    Domain(Box<StoredDomain>),
    /// A standalone composite type, whose relation follows it.
    Composite(Box<StoredComposite>),
}

impl TypeObject {
    #[must_use]
    pub fn oid(&self) -> u32 {
        match self {
            Self::Enum(definition) => definition.oid,
            Self::Domain(domain) => domain.oid,
            Self::Composite(definition) => definition.oid,
        }
    }

    #[must_use]
    pub fn identity(&self) -> &RelationIdentity {
        match self {
            Self::Enum(definition) => &definition.identity,
            Self::Domain(domain) => &domain.identity,
            Self::Composite(definition) => &definition.identity,
        }
    }

    #[must_use]
    pub fn owner(&self) -> RoleIdentity {
        match self {
            Self::Enum(definition) => definition.owner,
            Self::Domain(domain) => domain.owner,
            Self::Composite(definition) => definition.owner,
        }
    }

    #[must_use]
    pub fn array_name(&self) -> String {
        match self {
            Self::Enum(definition) => definition.array_name.clone(),
            Self::Domain(domain) => domain.array_type_name(),
            Self::Composite(definition) => definition.array_name.clone(),
        }
    }

    #[must_use]
    pub fn usage_acl(&self) -> Option<&[ObjectAclEntry]> {
        match self {
            Self::Enum(definition) => definition.usage_acl.as_deref(),
            Self::Domain(domain) => domain.usage_acl.as_deref(),
            Self::Composite(definition) => definition.usage_acl.as_deref(),
        }
    }
}

/// What a type name resolved to before command-specific checks.
#[derive(Debug, Clone)]
enum ResolvedKind {
    User(TypeObject),
    /// The generated array type of a user-defined type or relation row type.
    Array {
        element: u32,
    },
    RowType(RowTypeRelation),
    /// A type that no role created: built-in types and fixed system catalog domains.
    System,
}

#[derive(Debug, Clone)]
pub struct ResolvedTypeObject {
    oid: u32,
    kind: ResolvedKind,
}

/// Resolve a type name as `typenameTypeId` does: a missing schema is 3F000 and a schema without USAGE is 42501 before the type lookup reports 42704.
pub fn resolve_type_object(
    context: &TypeObjectBinding<'_>,
    name: &str,
) -> Result<ResolvedTypeObject, SQLError> {
    let parsed = crate::parse_regtype_name(name)?
        .ok_or_else(|| SQLError::Internal("ALTER TYPE has no type name".into()))?;
    let label = format!(
        "{}{}",
        parsed.names.join("."),
        "[]".repeat(parsed.array_dimensions)
    );
    if let [schema, _] = parsed.names.as_slice() {
        if context.catalog.schema_security(schema).is_none() {
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
    let oid = context
        .catalog
        .resolve_drop_type_oid(name)?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{label}\" does not exist"),
        })?;
    let oid = u32::try_from(oid)
        .map_err(|_| SQLError::Internal(format!("type OID {oid} is out of range")))?;
    Ok(ResolvedTypeObject {
        oid,
        kind: classify(context, oid),
    })
}

fn classify(context: &TypeObjectBinding<'_>, oid: u32) -> ResolvedKind {
    if let Some(domain) = context.catalog.domain_by_oid(oid) {
        return ResolvedKind::User(TypeObject::Domain(Box::new(domain)));
    }
    if let Some(definition) = context.catalog.enum_by_type_oid(oid) {
        if definition.array_oid == oid {
            return ResolvedKind::Array {
                element: definition.oid,
            };
        }
        return ResolvedKind::User(TypeObject::Enum(definition));
    }
    if let Some(definition) = context.catalog.composite_by_type_oid(oid) {
        if definition.array_oid == oid {
            return ResolvedKind::Array {
                element: definition.oid,
            };
        }
        return ResolvedKind::User(TypeObject::Composite(Box::new(definition)));
    }
    if let Some(element) = context.catalog.user_array_element(oid) {
        return ResolvedKind::Array { element };
    }
    if let Some(relation) = context.catalog.row_type_relation(oid) {
        return ResolvedKind::RowType(relation);
    }
    ResolvedKind::System
}

impl ResolvedTypeObject {
    #[must_use]
    pub fn oid(&self) -> u32 {
        self.oid
    }

    fn owner(&self, context: &TypeObjectBinding<'_>) -> RoleIdentity {
        match &self.kind {
            ResolvedKind::User(object) => object.owner(),
            ResolvedKind::Array { element } => context
                .catalog
                .domain_by_oid(*element)
                .map(|domain| domain.owner)
                .or_else(|| {
                    context
                        .catalog
                        .enum_by_type_oid(*element)
                        .map(|definition| definition.owner)
                })
                .or_else(|| {
                    context
                        .catalog
                        .composite_by_type_oid(*element)
                        .map(|definition| definition.owner)
                })
                .unwrap_or(RoleIdentity::BOOTSTRAP),
            ResolvedKind::RowType(relation) => relation.owner,
            ResolvedKind::System => RoleIdentity::BOOTSTRAP,
        }
    }

    /// `ALTER DOMAIN` and `GRANT ... ON DOMAIN` name only domains.
    pub fn require_domain_keyword(
        &self,
        context: &TypeObjectBinding<'_>,
        kind: TypeObjectKind,
    ) -> Result<(), SQLError> {
        if kind == TypeObjectKind::Domain
            && !matches!(self.kind, ResolvedKind::User(TypeObject::Domain(_)))
        {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("{} is not a domain", format_type(context, self.oid)?),
            });
        }
        Ok(())
    }

    /// `object_ownercheck` on the type: the current role must have the owner's privileges. An array type is owned by its element's owner.
    pub fn require_owner(&self, context: &TypeObjectBinding<'_>) -> Result<(), SQLError> {
        if context
            .authority
            .current_user_has_role_privileges(&self.owner(context))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "must be owner of type {}",
                format_type(context, self.array_element().unwrap_or(self.oid))?
            ),
        })
    }

    /// The relation of a table row type, which `SET SCHEMA` reports after its duplicate-name check.
    #[must_use]
    pub fn row_type_identity(&self) -> Option<RelationIdentity> {
        match &self.kind {
            ResolvedKind::RowType(relation) => Some(RelationIdentity::new(
                &relation.schema,
                &relation.local_name,
            )),
            _ => None,
        }
    }

    fn array_element(&self) -> Option<u32> {
        match self.kind {
            ResolvedKind::Array { element } => Some(element),
            _ => None,
        }
    }

    /// A table's row type is altered through `ALTER TABLE`.
    pub fn reject_row_type(&self, context: &TypeObjectBinding<'_>) -> Result<(), SQLError> {
        if let ResolvedKind::RowType(_) = self.kind {
            return Err(SQLError::Diagnostic {
                sqlstate: "42809".into(),
                message: format!("{} is a table's row type", format_type(context, self.oid)?),
                detail: None,
                hint: Some("Use ALTER TABLE instead.".into()),
            });
        }
        Ok(())
    }

    /// A generated array type follows its element type.
    pub fn reject_array(&self, context: &TypeObjectBinding<'_>) -> Result<(), SQLError> {
        if let Some(element) = self.array_element() {
            return Err(SQLError::Diagnostic {
                sqlstate: "42809".into(),
                message: format!(
                    "cannot alter array type {}",
                    format_type(context, self.oid)?
                ),
                detail: None,
                hint: Some(format!(
                    "You can alter type {}, which will alter the array type as well.",
                    format_type(context, element)?
                )),
            });
        }
        Ok(())
    }

    /// `ExecGrant_Type_check`: array types have no privileges of their own, and `ON DOMAIN` names only domains.
    pub fn require_grant_target(
        &self,
        context: &TypeObjectBinding<'_>,
        kind: TypeObjectKind,
    ) -> Result<(), SQLError> {
        if self.array_element().is_some() {
            return Err(SQLError::Diagnostic {
                sqlstate: "0LP01".into(),
                message: "cannot set privileges of array types".into(),
                detail: None,
                hint: Some("Set the privileges of the element type instead.".into()),
            });
        }
        if kind == TypeObjectKind::Domain
            && !matches!(self.kind, ResolvedKind::User(TypeObject::Domain(_)))
        {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{}\" is not a domain", format_type(context, self.oid)?),
            });
        }
        Ok(())
    }

    /// The user-defined type to change. Built-in types, system catalog domains and relation row types have fixed definitions here.
    pub fn into_type_object(self, context: &TypeObjectBinding<'_>) -> Result<TypeObject, SQLError> {
        match self.kind {
            ResolvedKind::User(object) => Ok(object),
            ResolvedKind::Array { .. } | ResolvedKind::RowType(_) | ResolvedKind::System => {
                Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!(
                        "type {} has a fixed system definition",
                        format_type(context, self.oid)?
                    ),
                })
            }
        }
    }
}

fn format_type(context: &TypeObjectBinding<'_>, oid: u32) -> Result<String, SQLError> {
    context
        .catalog
        .format_drop_type(i64::from(oid))
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::Internal(format!("type {oid} disappeared")))
}
