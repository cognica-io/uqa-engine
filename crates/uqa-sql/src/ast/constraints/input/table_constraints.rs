//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Produced, Values};
use crate::ast::{
    stored_input::{boolean, unit},
    TableConstraintSet,
};

mod fields;
use fields::retain;
pub(crate) use fields::ParsedTableConstraints;

impl<'a> Input<'a, '_> {
    pub(super) fn table_constraints(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<TableConstraintSet>, InputError> {
        self.table_constraint_fields(token, values)?
            .into_produced(&self.control)
    }

    pub(super) fn table_constraint_fields(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<ParsedTableConstraints, InputError> {
        let mut result = ParsedTableConstraints::new(&self.control)?;
        let constraints = &mut result.value;
        let memory = &mut result.memory;
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
                        constraints.dropped_attributes = retain(
                            &mut memory.dropped_attributes,
                            input.array(token, Self::dropped_column)?,
                        );
                    }
                    1 if token == JsonToken::Null => (),
                    1 => constraints.columns_declared = Some(boolean(token)?),
                    2 => {
                        constraints.checks = retain(
                            &mut memory.checks,
                            input.array(token, |input, token| input.table_check(token, values))?,
                        );
                    }
                    3 => {
                        constraints.foreign_keys = retain(
                            &mut memory.foreign_keys,
                            input.array(token, Self::table_foreign_key)?,
                        );
                    }
                    4 => {
                        constraints.key_constraints = retain(
                            &mut memory.key_constraints,
                            input.array(token, Self::table_key)?,
                        );
                    }
                    5 => constraints.persistence = unit(&input.unit_tag(token)?)?,
                    6 => constraints.on_commit = unit(&input.unit_tag(token)?)?,
                    7 => {
                        constraints.hierarchy =
                            retain(&mut memory.hierarchy, input.table_hierarchy(token, values)?);
                    }
                    8 if token == JsonToken::Null => (),
                    8 => constraints.catalog_oids = Some(input.relation_oids(token)?),
                    _ => {
                        constraints.row_type_array_name =
                            retain(&mut memory.row_type_array_name, input.optional_text(token)?);
                    }
                }
                Ok(())
            },
        )?;
        self.control.check()?;
        Ok(result)
    }
}
