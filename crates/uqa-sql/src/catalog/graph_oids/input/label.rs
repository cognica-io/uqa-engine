//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Labels preserve optional companion OIDs and the first owned spelling of duplicate column keys.

use super::{
    fields::{invalid, number, optional, Fields, Input},
    Budgeted, JsonToken, LabelCatalogOids, OwnedMap, SQLError,
};
use crate::catalog::{
    graph_oids::{EndpointIndexOids, IndexConstraintOids},
    relation_oids::RelationCatalogOids,
};

impl Input<'_, '_, '_, '_> {
    pub(super) async fn label(
        &mut self,
        first: JsonToken<'_, &[u8]>,
    ) -> Result<Budgeted<LabelCatalogOids>, SQLError> {
        let mut fields = Fields::new(
            first,
            &[
                "sequence",
                "relation",
                "id_default",
                "properties_default",
                "not_null",
                "toast_table",
                "toast_index",
                "primary_key",
                "endpoint_indexes",
                "trigger",
            ],
            0b10_0111_1111,
        )?;
        let mut numbers = [0; 6];
        let (mut relation, mut not_null, mut primary_key, mut endpoint_indexes) =
            (None, None, None, None);
        while let Some((field, token)) = fields.next(self).await? {
            match field {
                0 => numbers[0] = number(token)?,
                1 => relation = Some(self.relation(token).await?),
                2 => numbers[1] = number(token)?,
                3 => numbers[2] = number(token)?,
                4 => not_null = Some(self.not_null(token).await?),
                5 => numbers[3] = number(token)?,
                6 => numbers[4] = number(token)?,
                7 if token != JsonToken::Null => {
                    let [index, constraint] = self.pair(token, &["index", "constraint"]).await?;
                    primary_key = Some(IndexConstraintOids { index, constraint });
                }
                8 if token != JsonToken::Null => {
                    let [start_id, end_id] = self.pair(token, &["start_id", "end_id"]).await?;
                    endpoint_indexes = Some(EndpointIndexOids { start_id, end_id });
                }
                9 => numbers[5] = number(token)?,
                _ => (),
            }
        }
        let relation = relation.ok_or_else(invalid)?;
        let (not_null, memory) = not_null.ok_or_else(invalid)?.into_parts();
        Ok(Budgeted::new(
            LabelCatalogOids {
                sequence: numbers[0],
                relation,
                id_default: numbers[1],
                properties_default: numbers[2],
                not_null,
                toast_table: numbers[3],
                toast_index: numbers[4],
                primary_key,
                endpoint_indexes,
                trigger: numbers[5],
            },
            memory,
        ))
    }

    async fn relation(
        &mut self,
        first: JsonToken<'_, &[u8]>,
    ) -> Result<RelationCatalogOids, SQLError> {
        let mut fields = Fields::new(first, &["relation", "row_type", "array_type", "rule"], 1)?;
        let (mut relation, mut row_type, mut array_type, mut rule) = (0, None, None, None);
        while let Some((field, token)) = fields.next(self).await? {
            match field {
                0 => relation = number(token)?,
                1 => row_type = optional(token)?,
                2 => array_type = optional(token)?,
                3 => rule = optional(token)?,
                _ => unreachable!(),
            }
        }
        Ok(RelationCatalogOids {
            relation,
            row_type,
            array_type,
            rule,
        })
    }

    async fn pair(
        &mut self,
        first: JsonToken<'_, &[u8]>,
        names: &'static [&'static str; 2],
    ) -> Result<[u32; 2], SQLError> {
        let mut fields = Fields::new(first, names, 3)?;
        let mut values = [0; 2];
        while let Some((field, token)) = fields.next(self).await? {
            values[field] = number(token)?;
        }
        Ok(values)
    }

    async fn not_null(
        &mut self,
        first: JsonToken<'_, &[u8]>,
    ) -> Result<Budgeted<OwnedMap<String, u32>>, SQLError> {
        if first != JsonToken::StartObject {
            return Err(invalid());
        }
        let mut memory = self.memory().empty_reservation();
        let mut values = OwnedMap::<String, u32>::new();
        loop {
            let key = self.next().await?;
            if key == JsonToken::EndObject {
                break;
            }
            if !matches!(key, JsonToken::Key(_)) {
                return Err(invalid());
            }
            let key = self.text(key).await?;
            let value = number(self.next().await?)?;
            if let Some(previous) = values.get_mut(key.as_str()) {
                *previous = value;
            } else {
                memory.grow(OwnedMap::<String, u32>::entry_bytes())?;
                let (key, payload) = key.into_parts();
                memory.absorb(payload);
                values.insert(key, value);
            }
        }
        Ok(Budgeted::new(values, memory))
    }
}
