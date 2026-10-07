//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Freeze predecessor composite constructors while the initial catalog transaction owns publication.

use super::{publish, DomainRegistryPublication};
use uqa_sql::{schema::dependencies::registration::SchemaDependencyBindingContext, SQLError};

pub fn restore_constructors(
    context: &SchemaDependencyBindingContext<'_>,
    publication: &dyn DomainRegistryPublication,
    allow_migration: bool,
) -> Result<(), SQLError> {
    let before = publication.domain_registry().clone();
    if before.is_empty() {
        return Ok(());
    }
    let mut candidate = before.clone();
    let mut changed = false;
    let binding = context.bindings.binding_scope()?;
    let binding = binding.context();
    let schema = uqa_sql::schema::SchemaBindingContext {
        catalog: context.schema,
        binding: &binding,
    };
    for domain in candidate.values_mut() {
        changed |= uqa_sql::schema::domains::restore_composite_constructors(
            &schema,
            &mut domain.definition,
        )?;
    }
    if changed {
        if !allow_migration {
            return Err(SQLError::Internal(
                "domain constructors require an initial-open migration".into(),
            ));
        }
        publish(publication, &before, candidate)?;
    }
    Ok(())
}
