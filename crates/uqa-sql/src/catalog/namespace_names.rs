//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native relation and type declarations claim separate catalog name spaces without rebinding.

use super::work::{compare_text, inline, Work};
use crate::SQLError;
use std::cmp::Ordering;
use uqa_core::{
    memory::{Budgeted, MapKeyInput, MemoryBudget, OwnedMap, Produced, ProductionControl},
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

type Key = (Produced<String>, Produced<String>);
type Names = OwnedMap<Key, ()>;

/// Reject duplicate or fixed relation/type names across the supplied complete native declarations. All claims remain private and are discarded together on failure; no catalog names are changed or generated arrays displaced.
pub fn validate<'a>(
    declarations: impl Iterator<Item = NativeNameInput<'a>>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    inline(validate_with_work(
        declarations,
        memory,
        cancellation,
        &mut (),
    ))
}

/// Validate and retire the same ordered names through the caller's work channel.
pub async fn validate_with_work<'a, W: Work + Send>(
    declarations: impl Iterator<Item = NativeNameInput<'a>>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut W,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut relations = Names::new();
    let mut types = Names::new();
    let mut claims = Claims {
        memory,
        control: &control,
        work,
    };
    let result = async {
        for declaration in declarations {
            claims.checkpoint().await?;
            match declaration {
                NativeNameInput::Relation(identity) => {
                    claims.relation(&mut relations, identity).await?;
                }
                NativeNameInput::RowRelation(identity, array) => {
                    claims.relation(&mut relations, identity).await?;
                    claims.data_type(&mut types, identity).await?;
                    claims.array_type(&mut types, identity, array).await?;
                }
                NativeNameInput::Type(identity) => claims.data_type(&mut types, identity).await?,
                NativeNameInput::TypeWithArray(identity, array) => {
                    claims.data_type(&mut types, identity).await?;
                    claims.array_type(&mut types, identity, array).await?;
                }
            }
        }
        control.check()?;
        Ok(())
    }
    .await;
    for map in [relations, types] {
        super::work::retire_entries(map.into_iter(), &control, work).await?;
    }
    result
}

struct Claims<'a, 'c, W> {
    memory: &'a MemoryBudget,
    control: &'a ProductionControl<'c>,
    work: &'a mut W,
}

impl<W: Work + Send> MapKeyInput<Key> for Claims<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.work.checkpoint().await?;
        self.control.check()?;
        Ok(())
    }

    async fn compare(&mut self, query: &Key, stored: &Key) -> Result<Ordering, SQLError> {
        let schema = compare_text(self.work, &query.0, &stored.0, self.control).await?;
        if schema.is_eq() {
            compare_text(self.work, &query.1, &stored.1, self.control).await
        } else {
            Ok(schema)
        }
    }
}

impl<W: Work + Send> Claims<'_, '_, W> {
    async fn relation(
        &mut self,
        names: &mut Names,
        identity: &RelationIdentity,
    ) -> Result<(), SQLError> {
        if super::SystemRelation::at(&identity.schema, &identity.name).is_some() {
            return Err(invalid("fixed relation"));
        }
        let name = self.work.text(&identity.name, self.control).await?;
        self.insert(names, &identity.schema, name).await
    }

    async fn data_type(
        &mut self,
        names: &mut Names,
        identity: &RelationIdentity,
    ) -> Result<(), SQLError> {
        let name = self.work.text(&identity.name, self.control).await?;
        self.insert_type(names, &identity.schema, name).await
    }

    async fn array_type(
        &mut self,
        names: &mut Names,
        identity: &RelationIdentity,
        recorded: Option<&str>,
    ) -> Result<(), SQLError> {
        let name = match recorded {
            Some(name) => self.work.text(name, self.control).await?,
            None => super::array_type_names::default_array_type_name_with_control(
                &identity.name,
                self.control,
            )?,
        };
        self.insert_type(names, &identity.schema, name).await
    }

    async fn insert_type(
        &mut self,
        names: &mut Names,
        schema: &str,
        name: Produced<String>,
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
        self.insert(names, schema, name).await
    }

    async fn insert(
        &mut self,
        names: &mut Names,
        schema: &str,
        name: Produced<String>,
    ) -> Result<(), SQLError> {
        let schema = self.work.text(schema, self.control).await?;
        let key = Budgeted::new((schema, name), self.memory.reserve(0)?);
        if names
            .insert_admitted_key_from(key, (), self)
            .await?
            .is_some()
        {
            return Err(invalid("duplicate declaration"));
        }
        Ok(())
    }
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native catalog name: {reason}"))
}

#[cfg(test)]
mod tests;
