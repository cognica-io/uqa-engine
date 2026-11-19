//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ColumnDef, Input, InputError, JsonToken, Parts, Produced, Values};
use crate::ast::{
    stored_input::{boolean, optional, unit},
    ColumnType, GeneratedColumn,
};

impl<'a> Input<'a, '_> {
    pub(super) fn column_definition(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<ColumnDef>, InputError> {
        // The partial declaration drops before the combined lease on every failed field read.
        let mut parts = Parts::new(self.control);
        let mut column = ColumnDef::nullable(String::new(), ColumnType::Text);
        self.fields_with_defaults(
            token,
            &[
                "name",
                "ty",
                "collation_oid",
                "object_id",
                "attribute_number",
                "missing_value",
                "primary_key",
                "not_null",
                "not_null_explicit",
                "not_null_name",
                "not_null_identity",
                "not_null_validated",
                "not_null_no_inherit",
                "not_null_is_local",
                "auto_increment",
                "unique",
                "default",
                "generated",
                "default_catalog_oid",
                "check",
                "check_name",
                "check_enforced",
                "check_validated",
                "check_no_inherit",
                "check_is_local",
                "check_object_id",
                "check_catalog_oid",
                "references",
            ],
            0b1100_0011,
            0b1100_0011,
            |input, index, token| {
                match index {
                    0 => column.name = parts.take(input.text(token)?),
                    1 => column.ty = parts.take(input.column(token)?),
                    2 => column.collation_oid = optional(token)?,
                    3 => column.object_id = input.optional_identity(token)?,
                    4 => column.attribute_number = optional(token)?,
                    5 => {
                        column.missing_value = parts
                            .take(input.maybe(token, |input, token| values.value(input, token))?);
                    }
                    6 => column.primary_key = boolean(token)?,
                    7 => column.not_null = boolean(token)?,
                    8 => column.not_null_explicit = boolean(token)?,
                    9 => column.not_null_name = parts.take(input.optional_text(token)?),
                    10 => column.not_null_identity = input.optional_catalog_identity(token)?,
                    11 => column.not_null_validated = boolean(token)?,
                    12 => column.not_null_no_inherit = boolean(token)?,
                    13 => column.not_null_is_local = boolean(token)?,
                    14 => column.auto_increment = parts.take(input.auto_increment(token)?),
                    15 => column.unique = boolean(token)?,
                    16 => {
                        column.default =
                            parts.take(input.maybe(token, |input, token| {
                                input.schema_expression(token, values)
                            })?);
                    }
                    17 => {
                        column.generated =
                            parts.take(input.maybe(token, |input, token| {
                                input.generated_column(token, values)
                            })?);
                    }
                    18 => column.default_catalog_oid = optional(token)?,
                    19 => {
                        column.check =
                            parts.take(input.maybe(token, |input, token| {
                                input.schema_expression(token, values)
                            })?);
                    }
                    20 => column.check_name = parts.take(input.optional_text(token)?),
                    21 => column.check_enforced = boolean(token)?,
                    22 => column.check_validated = boolean(token)?,
                    23 => column.check_no_inherit = boolean(token)?,
                    24 => column.check_is_local = boolean(token)?,
                    25 => column.check_object_id = input.optional_identity(token)?,
                    26 => column.check_catalog_oid = optional(token)?,
                    _ => {
                        column.references =
                            parts.take(input.maybe(token, Self::foreign_key_reference)?);
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(column)
    }

    fn generated_column(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<GeneratedColumn>, InputError> {
        let (mut kind, mut expression, mut dependencies) = (None, None, None);
        self.fields_with_defaults(
            token,
            &["kind", "expression", "function_dependencies"],
            3,
            3,
            |input, index, token| {
                match index {
                    0 => kind = Some(unit(&input.unit_tag(token)?)?),
                    1 => {
                        expression =
                            Some(input.boxed(token, |input, token| {
                                input.schema_expression(token, values)
                            })?);
                    }
                    _ => dependencies = Some(input.array(token, Self::function_binding)?),
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = GeneratedColumn {
            kind: kind.expect("required generation kind"),
            expression: parts.take(expression.expect("required generation expression")),
            function_dependencies: dependencies
                .map(|value| parts.take(value))
                .unwrap_or_default(),
        };
        parts.finish(value)
    }
}
