//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain constraint changes retain the type identity and publish through the caller's catalog transaction.

use super::validation::{validate_values, DomainValueValidationContext};
use crate::catalog::domain;
use crate::schema::{deletion, types::TypeLifecycleContext};
use uqa_sql::{
    ast::{
        AlterDomain, AlterDomainAction, CreateDomain, DomainCheck, DomainNotNull, TypeObjectKind,
    },
    catalog::domain::StoredDomain,
    schema::type_objects::TypeObject,
    SQLError, SQLNotice,
};

pub use super::validation::{DomainValidationInputs, DomainValidationTables};

/// Bind only the new constraint against the domain's base type and current schema names.
pub trait DomainConstraintBinding {
    fn bind_added_check(
        &self,
        domain: &CreateDomain,
        check: DomainCheck,
    ) -> Result<DomainCheck, SQLError>;
    fn bind_added_not_null(
        &self,
        domain: &CreateDomain,
        constraint: DomainNotNull,
    ) -> Result<DomainNotNull, SQLError>;
}

pub struct DomainAlterContext<'a, S: Clone + 'static> {
    pub types: TypeLifecycleContext<'a>,
    pub bindings: &'a dyn DomainConstraintBinding,
    pub validation: DomainValidationInputs<'a, S>,
    pub deletion: &'a dyn deletion::CatalogRemovalInputs,
}

pub fn alter_domain<S: Clone + 'static>(
    context: &DomainAlterContext<'_, S>,
    statement: AlterDomain,
) -> Result<(), SQLError> {
    context.types.writer.prepare_writer()?;
    let resolved = crate::schema::types::lock_named_type(&context.types, &statement.name)?;
    resolved.require_domain_keyword(&context.types.binding, TypeObjectKind::Domain)?;
    resolved.require_owner(&context.types.binding)?;
    let TypeObject::Domain(mut domain) = resolved.into_type_object(&context.types.binding)? else {
        return Err(SQLError::Internal(
            "ALTER DOMAIN resolved a non-domain".into(),
        ));
    };
    let validation = DomainValueValidationContext {
        inputs: &context.validation,
        types: context.types.binding.catalog,
    };
    match statement.action {
        AlterDomainAction::AddCheck { constraint } => {
            let check = context
                .bindings
                .bind_added_check(&domain.definition, *constraint)?;
            domain.definition.checks.push(check.clone());
            domain
                .definition
                .checks
                .sort_by(|left, right| left.name.cmp(&right.name));
            publish(context, &mut domain)?;
            if check.validated {
                validate_values(&validation, &domain, Some(&check))?;
            }
        }
        AlterDomainAction::AddNotNull { constraint } => {
            // AlterDomainAddConstraint returns before considering a new name when typnotnull is already set.
            if domain.definition.not_null.is_some() {
                return Ok(());
            }
            domain.definition.not_null = Some(
                context
                    .bindings
                    .bind_added_not_null(&domain.definition, constraint)?,
            );
            publish(context, &mut domain)?;
            validate_values(&validation, &domain, None)?;
        }
        AlterDomainAction::DropConstraint {
            name,
            if_exists,
            cascade,
        } => {
            drop_constraint(context, &domain, &statement.name, &name, if_exists, cascade)?;
        }
        AlterDomainAction::ValidateConstraint { name } => {
            let Some(index) = domain
                .definition
                .checks
                .iter()
                .position(|check| check.name.as_deref() == Some(&name))
            else {
                let is_not_null = domain
                    .definition
                    .not_null
                    .as_ref()
                    .is_some_and(|check| check.name.as_deref() == Some(&name));
                return Err(SQLError::Routine {
                    sqlstate: if is_not_null { "42809" } else { "42704" }.into(),
                    message: if is_not_null {
                        format!(
                            "constraint \"{name}\" of domain \"{}\" is not a check constraint",
                            statement.name
                        )
                    } else {
                        missing_constraint(&statement.name, &name)
                    },
                });
            };
            // PostgreSQL repeats validation even when convalidated was already true.
            validate_values(&validation, &domain, Some(&domain.definition.checks[index]))?;
            domain.definition.checks[index].validated = true;
            publish(context, &mut domain)?;
        }
    }
    Ok(())
}

fn drop_constraint<S: Clone + 'static>(
    context: &DomainAlterContext<'_, S>,
    domain: &StoredDomain,
    written_name: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let exists = domain
        .definition
        .checks
        .iter()
        .any(|check| check.name.as_deref() == Some(name))
        || domain
            .definition
            .not_null
            .as_ref()
            .is_some_and(|check| check.name.as_deref() == Some(name));
    if !exists {
        let message = missing_constraint(written_name, name);
        if !if_exists {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message,
            });
        }
        context
            .types
            .notices
            .notice(SQLNotice::notice(format!("{message}, skipping")));
        return Ok(());
    }
    deletion::perform_deletion(
        &context.deletion.catalog_removal_context(),
        |dependencies| {
            Ok(vec![deletion::required_address(
                dependencies.domain_constraint_address(domain.oid, name),
                || format!("domain constraint {name}"),
            )?])
        },
        cascade,
    )?;
    Ok(())
}

fn missing_constraint(domain: &str, name: &str) -> String {
    format!("constraint \"{name}\" of domain \"{domain}\" does not exist")
}

fn publish<S: Clone + 'static>(
    context: &DomainAlterContext<'_, S>,
    domain: &mut StoredDomain,
) -> Result<(), SQLError> {
    let mut allocator = context
        .types
        .identities
        .allocator(crate::catalog::identity::allocate_catalog_object_id);
    uqa_sql::schema::domains::constraints::materialize(&mut domain.definition, &mut allocator)
        .map_err(uqa_sql::schema::constraint_metadata::ConstraintMetadataError::into_sql_error)?;
    let before = context.types.registries.domains.domain_registry().clone();
    let mut registry = before.clone();
    registry.insert(domain.identity.qualified_name(), domain.clone());
    domain::publish(context.types.registries.domains, &before, registry)?;
    context.types.changes.catalog_registry_changed();
    Ok(())
}
