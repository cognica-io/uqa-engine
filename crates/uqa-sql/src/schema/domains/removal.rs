//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP DOMAIN name binding, namespace privileges, and owner diagnostics.

use crate::{
    catalog::{domain::DomainCatalog, roles::RoleReferenceNames, security::SchemaSecurity},
    SQLError,
};

pub trait DomainDropCatalog: DomainCatalog {
    fn schema_security(&self, name: &str) -> Option<SchemaSecurity>;
    fn resolve_domain_drop_type(&self, name: &str) -> Result<Option<i64>, SQLError>;
    fn format_domain_drop_type(&self, oid: i64) -> Result<Option<String>, String>;
}
pub trait DomainDropAuthority {
    fn schema_usage(&self, schema: &str, role: &str) -> bool;
    fn current_user_has_role_privileges(&self, role: &str) -> bool;
}
pub struct DomainDropBinding<'a> {
    pub catalog: &'a dyn DomainDropCatalog,
    pub authority: &'a dyn DomainDropAuthority,
    pub session: &'a dyn RoleReferenceNames,
}
pub enum BoundDomainDrop {
    Target(u32),
    Skipped(String),
}

pub fn resolve_drop_domain(
    context: &DomainDropBinding<'_>,
    name: &str,
    if_exists: bool,
) -> Result<BoundDomainDrop, SQLError> {
    let parsed = crate::parse_regtype_name(name)?
        .ok_or_else(|| SQLError::Internal("DROP DOMAIN has no type name".into()))?;
    let label = format!(
        "{}{}",
        parsed.names.join("."),
        "[]".repeat(parsed.array_dimensions)
    );
    if let [schema, _] = parsed.names.as_slice() {
        if context.catalog.schema_security(schema).is_none() {
            if if_exists {
                return Ok(BoundDomainDrop::Skipped(format!(
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
            .schema_usage(schema, &context.session.current_user_name())
        {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("permission denied for schema {schema}"),
            });
        }
    }
    let oid = context.catalog.resolve_domain_drop_type(name)?;
    let Some(oid) = oid else {
        if if_exists {
            return Ok(BoundDomainDrop::Skipped(format!(
                "type \"{label}\" does not exist, skipping"
            )));
        }
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{label}\" does not exist"),
        });
    };
    let domain = u32::try_from(oid)
        .ok()
        .and_then(|oid| context.catalog.domain_by_oid(oid))
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("\"{label}\" is not a domain"),
        })?;
    let owns_schema = context
        .catalog
        .schema_security(&domain.identity.schema)
        .is_some_and(|security| {
            context
                .authority
                .current_user_has_role_privileges(&security.role_owner)
        });
    if !owns_schema
        && !context
            .authority
            .current_user_has_role_privileges(&domain.owner)
    {
        let label = context
            .catalog
            .format_domain_drop_type(oid)
            .map_err(SQLError::Internal)?
            .ok_or_else(|| SQLError::Internal("DROP DOMAIN target disappeared".into()))?;
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of type {label}"),
        });
    }
    Ok(BoundDomainDrop::Target(domain.oid))
}
