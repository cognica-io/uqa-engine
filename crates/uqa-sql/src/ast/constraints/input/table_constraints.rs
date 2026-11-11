//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced, Values};
use crate::ast::{
    stored_input::{boolean, unit},
    TableConstraintSet,
};

impl<'a> Input<'a, '_> {
    pub(super) fn table_constraints(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<TableConstraintSet>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut constraints = TableConstraintSet::default();
        self.fields_with_defaults(
            token,
            &[
                "dropped_attributes",
                "columns_declared",
                "checks",
                "foreign_keys",
                "key_constraints",
                "persistence",
                "on_commit",
                "hierarchy",
                "catalog_oids",
                "row_type_array_name",
            ],
            0,
            0,
            |input, index, token| {
                match index {
                    0 => {
                        constraints.dropped_attributes =
                            parts.take(input.array(token, Self::dropped_column)?);
                    }
                    1 if token == JsonToken::Null => (),
                    1 => constraints.columns_declared = Some(boolean(token)?),
                    2 => {
                        constraints.checks = parts.take(
                            input.array(token, |input, token| input.table_check(token, values))?,
                        );
                    }
                    3 => {
                        constraints.foreign_keys =
                            parts.take(input.array(token, Self::table_foreign_key)?);
                    }
                    4 => {
                        constraints.key_constraints =
                            parts.take(input.array(token, Self::table_key)?);
                    }
                    5 => constraints.persistence = unit(&input.unit_tag(token)?)?,
                    6 => constraints.on_commit = unit(&input.unit_tag(token)?)?,
                    7 => constraints.hierarchy = parts.take(input.table_hierarchy(token, values)?),
                    8 if token == JsonToken::Null => (),
                    8 => constraints.catalog_oids = Some(input.relation_oids(token)?),
                    _ => constraints.row_type_array_name = parts.take(input.optional_text(token)?),
                }
                Ok(())
            },
        )?;
        parts.finish(constraints)
    }
}
