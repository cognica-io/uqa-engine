//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced, Values};
use crate::ast::{
    stored_input::{boolean, optional, unit},
    DetachedPartitionConstraint, TableCheck, TableKeyConstraint, TableKeyConstraintKind,
};

impl<'a> Input<'a, '_> {
    pub(super) fn table_check(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<TableCheck>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = TableCheck {
            name: None,
            object_id: None,
            catalog_oid: None,
            is_local: true,
            expr: crate::ast::Expr::Star,
            enforced: true,
            validated: true,
            no_inherit: false,
            partition_constraint: None,
        };
        self.fields_with_defaults(
            token,
            &[
                "name",
                "object_id",
                "catalog_oid",
                "is_local",
                "expr",
                "enforced",
                "validated",
                "no_inherit",
                "partition_constraint",
            ],
            16,
            17,
            |input, index, token| {
                match index {
                    0 => value.name = parts.take(input.optional_text(token)?),
                    1 => value.object_id = input.optional_identity(token)?,
                    2 => value.catalog_oid = optional(token)?,
                    3 => value.is_local = boolean(token)?,
                    4 => value.expr = parts.take(input.schema_expression(token, values)?),
                    5 => value.enforced = boolean(token)?,
                    6 => value.validated = boolean(token)?,
                    7 => value.no_inherit = boolean(token)?,
                    _ => {
                        value.partition_constraint =
                            parts.take(input.maybe(token, |input, token| {
                                input.detached_partition(token, values)
                            })?);
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn detached_partition(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<DetachedPartitionConstraint>, InputError> {
        let (mut spec, mut bound) = (None, None);
        self.fields(token, &["spec", "bound"], 3, |input, index, token| {
            if index == 0 {
                spec = Some(input.partition_spec(token, values)?);
            } else {
                bound = Some(input.partition_bound(token, values)?);
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = DetachedPartitionConstraint {
            spec: parts.take(spec.expect("required partition specification")),
            bound: parts.take(bound.expect("required partition bound")),
        };
        parts.finish(value)
    }

    pub(super) fn table_key(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<TableKeyConstraint>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = TableKeyConstraint {
            catalog_identity: None,
            index_identity: None,
            name: None,
            kind: TableKeyConstraintKind::PrimaryKey,
            columns: Vec::new(),
            included_columns: Vec::new(),
            nulls_not_distinct: false,
            without_overlaps: false,
        };
        self.fields_with_defaults(
            token,
            &[
                "catalog_identity",
                "index_identity",
                "name",
                "kind",
                "columns",
                "included_columns",
                "nulls_not_distinct",
                "without_overlaps",
            ],
            24,
            28,
            |input, index, token| {
                match index {
                    0 => value.catalog_identity = input.optional_catalog_identity(token)?,
                    1 => value.index_identity = input.optional_catalog_identity(token)?,
                    2 => value.name = parts.take(input.optional_text(token)?),
                    3 => value.kind = unit(&input.unit_tag(token)?)?,
                    4 => value.columns = parts.take(input.texts(token)?),
                    5 => value.included_columns = parts.take(input.texts(token)?),
                    6 => value.nulls_not_distinct = boolean(token)?,
                    _ => value.without_overlaps = boolean(token)?,
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}
