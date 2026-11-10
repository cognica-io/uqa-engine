//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native relation and type declarations claim separate catalog name spaces without rebinding.

use crate::SQLError;
use uqa_core::{
    memory::{BudgetedMap, MemoryBudget, Produced, ProductionControl},
    CancellationToken, RelationIdentity,
};

/// Original declarations, including the generated array names already selected by their owners. A missing recorded array name retains the ordinary legacy first-name rule.
pub enum NativeNameInput<'a> {
    /// Sequences and indexes occupy only the relation namespace.
    Relation(&'a RelationIdentity),
    /// Tables, views, foreign tables and standalone composites also own a row type and array.
    RowRelation(&'a RelationIdentity, Option<&'a str>),
    /// A shell declaration has no generated array.
    Type(&'a RelationIdentity),
    /// A declared scalar or range family type owns its independently named array.
    TypeWithArray(&'a RelationIdentity, Option<&'a str>),
}

type Names = BudgetedMap<(Produced<String>, Produced<String>), ()>;

/// Reject duplicate or fixed relation/type names across the supplied complete native declarations. All claims remain private and are discarded together on failure; no catalog names are changed or generated arrays displaced.
pub fn validate<'a>(
    declarations: impl Iterator<Item = NativeNameInput<'a>>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut relations = Names::new(memory);
    let mut types = Names::new(memory);
    for declaration in declarations {
        control.check()?;
        match declaration {
            NativeNameInput::Relation(identity) => relation(&mut relations, identity, &control)?,
            NativeNameInput::RowRelation(identity, array) => {
                relation(&mut relations, identity, &control)?;
                data_type(&mut types, identity, &control)?;
                array_type(&mut types, identity, array, &control)?;
            }
            NativeNameInput::Type(identity) => data_type(&mut types, identity, &control)?,
            NativeNameInput::TypeWithArray(identity, array) => {
                data_type(&mut types, identity, &control)?;
                array_type(&mut types, identity, array, &control)?;
            }
        }
    }
    control.check()?;
    Ok(())
}

fn relation(
    names: &mut Names,
    identity: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if super::SystemRelation::at(&identity.schema, &identity.name).is_some() {
        return Err(invalid("fixed relation"));
    }
    insert(
        names,
        &identity.schema,
        control.copy_text(&identity.name)?,
        control,
    )
}

fn data_type(
    names: &mut Names,
    identity: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    insert_type(
        names,
        &identity.schema,
        control.copy_text(&identity.name)?,
        control,
    )
}

fn array_type(
    names: &mut Names,
    identity: &RelationIdentity,
    recorded: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let name = match recorded {
        Some(name) => control.copy_text(name)?,
        None => {
            super::array_type_names::default_array_type_name_with_control(&identity.name, control)?
        }
    };
    insert_type(names, &identity.schema, name, control)
}

fn insert_type(
    names: &mut Names,
    schema: &str,
    name: Produced<String>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if super::SystemRelation::at(schema, &name).is_some()
        || super::type_production::is_system_catalog_type_name(schema, &name)
        || (schema == "pg_catalog"
            && (super::type_metadata::builtin_scalar_type_by_name(&name).is_some()
                || super::type_metadata::routine_pseudo_type_oid(&name).is_some()
                || crate::ast::builtin_array_element_name(&name).is_some()))
    {
        return Err(invalid("fixed type"));
    }
    insert(names, schema, name, control)
}

fn insert(
    names: &mut Names,
    schema: &str,
    name: Produced<String>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if names
        .insert((control.copy_text(schema)?, name), ())?
        .is_some()
    {
        return Err(invalid("duplicate declaration"));
    }
    Ok(())
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native catalog name: {reason}"))
}

#[cfg(test)]
mod tests;
