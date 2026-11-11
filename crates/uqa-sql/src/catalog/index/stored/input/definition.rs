//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete stored index definitions retain the original syntax and allocation owners.

use crate::{
    ast::{
        stored_input::{boolean, Input, InputError, Parts, Values},
        IndexColumnOrder,
    },
    catalog::index::{IndexCatalogIdentity, IndexDefinition, IndexRelationships},
    SQLError,
};
use uqa_core::{
    json::JsonToken,
    memory::{MemoryBudget, Produced},
    CancellationToken,
};

/// Decode every native index-definition field, including types and scalar predicates, without resolving metadata or executing SQL. An absent legacy definition retains the original empty declaration.
pub fn definition(
    text: Option<&str>,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Produced<IndexDefinition>, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    let control = values.control();
    let Some(text) = text else {
        return Ok(control.finish(IndexDefinition::default(), control.empty_reservation())?);
    };
    let mut input = Input::new(text, control);
    let token = input.next()?;
    let output = input.index_definition(token, &values)?;
    input.end()?;
    Ok(output)
}

impl<'a> Input<'a, '_> {
    fn index_definition(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<IndexDefinition>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = IndexDefinition::default();
        self.closed_fields(
            token,
            &[
                "catalog",
                "relationships",
                "included_columns",
                "column_order",
                "key_names",
                "key_types",
                "predicate",
                "unique",
                "nulls_not_distinct",
            ],
            [384, 384],
            |input, index, token| {
                match index {
                    0 => value.catalog = parts.take(input.maybe(token, Self::index_catalog)?),
                    1 => value.relationships = input.index_relationships(token)?,
                    2 => value.included_columns = parts.take(input.texts(token)?),
                    3 => {
                        value.column_order = parts.take(input.array(token, Self::index_order)?);
                    }
                    4 => value.key_names = parts.take(input.texts(token)?),
                    5 => value.key_types = parts.take(input.array(token, Self::column)?),
                    6 => {
                        value.predicate = parts.take(input.maybe(token, |input, token| {
                            input
                                .boxed(token, |input, token| input.schema_expression(token, values))
                        })?);
                    }
                    7 => value.unique = boolean(token)?,
                    _ => value.nulls_not_distinct = boolean(token)?,
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn index_catalog(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<IndexCatalogIdentity>, InputError> {
        let (mut identity, mut table_object_id, mut physical_key) = (None, [0; 16], None);
        self.closed_fields(
            token,
            &["identity", "table_object_id", "physical_key"],
            [7, 7],
            |input, index, token| {
                match index {
                    0 => identity = Some(input.catalog_identity(token)?),
                    1 => table_object_id = input.identity(token)?,
                    _ => physical_key = Some(input.text(token)?),
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = IndexCatalogIdentity {
            identity: identity.expect("required index identity"),
            table_object_id,
            physical_key: parts.take(physical_key.expect("required physical index key")),
        };
        parts.finish(value)
    }

    fn index_relationships(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<IndexRelationships, InputError> {
        let mut value = IndexRelationships::default();
        self.closed_fields(
            token,
            &["owning_constraint", "parent_index"],
            [0, 0],
            |input, index, token| {
                let id = input.optional_identity(token)?;
                if index == 0 {
                    value.owning_constraint = id;
                } else {
                    value.parent_index = id;
                }
                Ok(())
            },
        )?;
        Ok(value)
    }

    fn index_order(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<IndexColumnOrder>, InputError> {
        let mut value = IndexColumnOrder::default();
        self.fields(
            token,
            &["descending", "nulls_first"],
            3,
            |_, index, token| {
                if index == 0 {
                    value.descending = boolean(token)?;
                } else {
                    value.nulls_first = boolean(token)?;
                }
                Ok(())
            },
        )?;
        self.scalar(value)
    }
}

#[cfg(test)]
mod tests;
