//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced};
use crate::{
    ast::stored_input::{boolean, number, optional},
    catalog::{relation_attributes::DroppedAttribute, relation_oids::RelationCatalogOids},
};

impl<'a> Input<'a, '_> {
    pub(super) fn relation_oids(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<RelationCatalogOids, InputError> {
        let mut value = RelationCatalogOids {
            relation: 0,
            row_type: None,
            array_type: None,
            rule: None,
        };
        self.closed_fields(
            token,
            &["relation", "row_type", "array_type", "rule"],
            [1, 1],
            |_, index, token| {
                match index {
                    0 => value.relation = number(token)?,
                    1 => value.row_type = optional(token)?,
                    2 => value.array_type = optional(token)?,
                    _ => value.rule = optional(token)?,
                }
                Ok(())
            },
        )?;
        Ok(value)
    }

    pub(super) fn dropped_column(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<DroppedAttribute>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = DroppedAttribute {
            number: 0,
            type_length: 0,
            type_modifier: 0,
            dimensions: 0,
            by_value: false,
            alignment: String::new(),
            storage: String::new(),
            collation: 0,
            identity: String::new(),
            is_local: false,
            inheritance_count: 0,
        };
        self.closed_fields(
            token,
            &[
                "number",
                "type_length",
                "type_modifier",
                "dimensions",
                "by_value",
                "alignment",
                "storage",
                "collation",
                "identity",
                "is_local",
                "inheritance_count",
            ],
            [2047, 2047],
            |input, index, token| {
                match index {
                    0 => value.number = number(token)?,
                    1 => value.type_length = number(token)?,
                    2 => value.type_modifier = number(token)?,
                    3 => value.dimensions = number(token)?,
                    4 => value.by_value = boolean(token)?,
                    5 => value.alignment = parts.take(input.text(token)?),
                    6 => value.storage = parts.take(input.text(token)?),
                    7 => value.collation = number(token)?,
                    8 => value.identity = parts.take(input.text(token)?),
                    9 => value.is_local = boolean(token)?,
                    _ => value.inheritance_count = number(token)?,
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}
