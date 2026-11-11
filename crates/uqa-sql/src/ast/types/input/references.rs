//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{scalar, ColumnType, InputError, JsonToken, Produced};
use crate::ast::stored_input::{number, Input};
use crate::ast::{CompositeTypeReference, EnumTypeReference, RangeAlignment, RangeTypeReference};

impl<'a> Input<'a, '_> {
    pub(super) fn reference(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
    ) -> Result<Produced<ColumnType>, InputError> {
        let names: &[&str] = match tag {
            "Enum" => &["schema", "name", "oid", "array_oid"],
            "Composite" => &["schema", "name", "oid", "array_oid", "relation_oid"],
            _ => &["schema", "name", "oid", "array_oid", "alignment"],
        };
        let (mut schema, mut name) = (None, None);
        let (mut oid, mut array_oid, mut relation_oid) = (0, 0, 0);
        let mut alignment = RangeAlignment::Integer;
        self.fields(
            token,
            names,
            (1 << names.len()) - 1,
            |input, index, token| {
                match index {
                    0 => schema = Some(input.text(token)?),
                    1 => name = Some(input.text(token)?),
                    2 => oid = number(token)?,
                    3 => array_oid = number(token)?,
                    4 if tag == "Composite" => relation_oid = number(token)?,
                    4 => alignment = scalar::alignment(&input.unit_tag(token)?)?,
                    _ => unreachable!("declared reference field"),
                }
                Ok(())
            },
        )?;
        let (schema, schema_memory) = schema.expect("required schema").into_parts();
        let (name, name_memory) = name.expect("required name").into_parts();
        let value = match tag {
            "Enum" => ColumnType::Enum(EnumTypeReference {
                schema,
                name,
                oid,
                array_oid,
            }),
            "Composite" => ColumnType::Composite(CompositeTypeReference {
                schema,
                name,
                oid,
                array_oid,
                relation_oid,
            }),
            _ => {
                let reference = RangeTypeReference {
                    schema,
                    name,
                    oid,
                    array_oid,
                    alignment,
                };
                if tag == "DeclaredRange" {
                    ColumnType::DeclaredRange(reference)
                } else {
                    ColumnType::DeclaredMultirange(reference)
                }
            }
        };
        Ok(self
            .control
            .finish(value, self.control.combine(schema_memory, name_memory))?)
    }
}
