//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced};
use crate::ast::{
    stored_input::{boolean, number, unit},
    ConstraintCatalogIdentity, ForeignKeyAction, ForeignKeyMatch, ForeignKeyRef,
    ReferencedPartitionConstraint,
};

impl<'a> Input<'a, '_> {
    pub(super) fn optional_identity(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Option<[u8; 16]>, InputError> {
        if token == JsonToken::Null {
            Ok(None)
        } else {
            self.identity(token).map(Some)
        }
    }

    pub(super) fn optional_catalog_identity(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Option<ConstraintCatalogIdentity>, InputError> {
        if token == JsonToken::Null {
            Ok(None)
        } else {
            self.catalog_identity(token).map(Some)
        }
    }

    fn catalog_identity(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<ConstraintCatalogIdentity, InputError> {
        let (mut object_id, mut oid) = ([0; 16], 0);
        self.closed_fields(
            token,
            &["object_id", "oid"],
            [3, 3],
            |input, index, token| {
                if index == 0 {
                    object_id = input.identity(token)?;
                } else {
                    oid = number(token)?;
                }
                Ok(())
            },
        )?;
        Ok(ConstraintCatalogIdentity { object_id, oid })
    }

    pub(super) fn foreign_key_reference(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<ForeignKeyRef>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut reference = ForeignKeyRef {
            referenced_index: None,
            referenced_key: None,
            name: None,
            object_id: None,
            catalog_identity: None,
            table: String::new(),
            column: None,
            on_update: ForeignKeyAction::NoAction,
            on_delete: ForeignKeyAction::NoAction,
            match_type: ForeignKeyMatch::Simple,
            enforced: true,
            validated: true,
            deferrable: false,
            initially_deferred: false,
            period: false,
            referenced_partitions: Vec::new(),
        };
        self.fields_with_defaults(
            token,
            &[
                "referenced_index",
                "referenced_key",
                "name",
                "object_id",
                "catalog_identity",
                "table",
                "column",
                "on_update",
                "on_delete",
                "match_type",
                "enforced",
                "validated",
                "deferrable",
                "initially_deferred",
                "period",
                "referenced_partitions",
            ],
            1 << 5,
            1 << 5,
            |input, index, token| {
                match index {
                    0 => reference.referenced_index = input.optional_identity(token)?,
                    1 => reference.referenced_key = parts.take(input.optional_text(token)?),
                    2 => reference.name = parts.take(input.optional_text(token)?),
                    3 => reference.object_id = input.optional_identity(token)?,
                    4 => reference.catalog_identity = input.optional_catalog_identity(token)?,
                    5 => reference.table = parts.take(input.text(token)?),
                    6 => reference.column = parts.take(input.optional_text(token)?),
                    7 => reference.on_update = unit(&input.unit_tag(token)?)?,
                    8 => reference.on_delete = unit(&input.unit_tag(token)?)?,
                    9 => reference.match_type = unit(&input.unit_tag(token)?)?,
                    10 => reference.enforced = boolean(token)?,
                    11 => reference.validated = boolean(token)?,
                    12 => reference.deferrable = boolean(token)?,
                    13 => reference.initially_deferred = boolean(token)?,
                    14 => reference.period = boolean(token)?,
                    _ => {
                        reference.referenced_partitions =
                            parts.take(input.array(token, Self::referenced_partition)?);
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(reference)
    }

    pub(super) fn referenced_partition(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<ReferencedPartitionConstraint>, InputError> {
        let (mut partition, mut parent, mut name, mut identity, mut validated) =
            ([0; 16], None, None, None, false);
        self.fields_with_defaults(
            token,
            &[
                "partition",
                "parent",
                "name",
                "catalog_identity",
                "validated",
            ],
            29,
            29,
            |input, index, token| {
                match index {
                    0 => partition = input.identity(token)?,
                    1 => parent = input.optional_identity(token)?,
                    2 => name = Some(input.text(token)?),
                    3 => identity = Some(input.catalog_identity(token)?),
                    _ => validated = boolean(token)?,
                }
                Ok(())
            },
        )?;
        let mut parts = Parts::new(self.control);
        let value = ReferencedPartitionConstraint {
            partition,
            parent,
            name: parts.take(name.expect("required partition constraint name")),
            catalog_identity: identity.expect("required partition constraint identity"),
            validated,
        };
        parts.finish(value)
    }
}
