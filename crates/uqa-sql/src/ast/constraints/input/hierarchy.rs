//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced, Values};
use crate::ast::{stored_input::number, PartitionIdentityOverride, TableHierarchy};

impl<'a> Input<'a, '_> {
    pub(super) fn table_hierarchy(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<TableHierarchy>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = TableHierarchy::default();
        self.fields_with_defaults(
            token,
            &[
                "parents",
                "parent_sequence_numbers",
                "partition_spec",
                "partition_bound",
                "local_columns",
                "partition_identity_overrides",
                "partition_inherited_key_constraints",
                "partition_inherited_foreign_keys",
            ],
            0,
            0,
            |input, index, token| {
                match index {
                    0 => value.parents = parts.take(input.texts(token)?),
                    1 => {
                        value.parent_sequence_numbers = parts
                            .take(input.array(token, |input, token| input.scalar(number(token)?))?);
                    }
                    2 => {
                        value.partition_spec = parts
                            .take(input.maybe(token, |input, token| {
                                input.partition_spec(token, values)
                            })?);
                    }
                    3 => {
                        value.partition_bound =
                            parts.take(input.maybe(token, |input, token| {
                                input.partition_bound(token, values)
                            })?);
                    }
                    4 => value.local_columns = parts.take(input.texts(token)?),
                    5 => {
                        value.partition_identity_overrides =
                            parts.take(input.array(token, Self::partition_identity_override)?);
                    }
                    6 => {
                        value.partition_inherited_key_constraints =
                            parts.take(input.array(token, Self::table_key)?);
                    }
                    _ => {
                        value.partition_inherited_foreign_keys =
                            parts.take(input.array(token, Self::table_foreign_key)?);
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn partition_identity_override(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<PartitionIdentityOverride>, InputError> {
        let (mut column, mut original) = (None, None);
        self.fields_with_defaults(
            token,
            &["column", "original"],
            1,
            1,
            |input, index, token| {
                if index == 0 {
                    column = Some(input.text(token)?);
                } else {
                    original = Some(input.maybe(token, Self::sequence_provenance)?);
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = PartitionIdentityOverride {
            column: parts.take(column.expect("required overridden column")),
            original: original.and_then(|value| parts.take(value)),
        };
        parts.finish(value)
    }
}
