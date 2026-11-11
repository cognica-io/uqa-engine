//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    json::{number, optional, Input},
    scalar, ColumnType, JsonToken, Produced, SQLError,
};

impl<'a> Input<'a, '_> {
    pub(super) fn numeric(&mut self, token: JsonToken<'a>) -> Result<ColumnType, SQLError> {
        let (mut precision, mut scale) = (None, None);
        self.fields(token, &["precision", "scale"], 0, |_, index, token| {
            if index == 0 {
                precision = optional(token)?;
            } else {
                scale = optional(token)?;
            }
            Ok(())
        })?;
        Ok(ColumnType::Numeric { precision, scale })
    }

    pub(super) fn interval(&mut self, token: JsonToken<'a>) -> Result<ColumnType, SQLError> {
        let (mut fields, mut precision) = (None, None);
        self.fields(token, &["fields", "precision"], 1, |input, index, token| {
            if index == 0 {
                fields = Some(scalar::interval(&input.unit_tag(token)?)?);
            } else {
                precision = optional(token)?;
            }
            Ok(())
        })?;
        Ok(ColumnType::IntervalWithFields {
            fields: fields.expect("required interval field"),
            precision,
        })
    }

    pub(super) fn domain(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        let (mut schema, mut name, mut base) = (None, None, None);
        let (mut oid, mut array_oid, mut collation_oid) = (0, None, None);
        self.fields(
            token,
            &[
                "schema",
                "name",
                "oid",
                "array_oid",
                "collation_oid",
                "base",
            ],
            0b10_0111,
            |input, index, token| {
                match index {
                    0 => schema = Some(input.text(token)?),
                    1 => name = Some(input.text(token)?),
                    2 => oid = number(token)?,
                    3 => array_oid = optional(token)?,
                    4 => collation_oid = optional(token)?,
                    5 => base = Some(input.column(token)?),
                    _ => unreachable!("declared domain field"),
                }
                Ok(())
            },
        )?;
        let box_memory = self.control.reserve(size_of::<ColumnType>())?;
        let (schema, schema_memory) = schema.expect("required schema").into_parts();
        let (name, name_memory) = name.expect("required name").into_parts();
        let (base, base_memory) = base.expect("required base").into_parts();
        let memory = self.control.combine(
            self.control.combine(schema_memory, name_memory),
            self.control.combine(base_memory, box_memory),
        );
        Ok(self.control.finish(
            ColumnType::Domain {
                schema,
                name,
                oid,
                array_oid,
                collation_oid,
                base: Box::new(base),
            },
            memory,
        )?)
    }
}
