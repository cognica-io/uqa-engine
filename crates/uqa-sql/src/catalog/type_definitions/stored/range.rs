//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    Input, InputError, JsonToken, Parts, Produced, RelationIdentity, RoleIdentity, StoredRange,
};
use crate::ast::stored_input::number;
use crate::{
    ast::{stored_input::optional, ColumnType},
    catalog::range_type::StoredMultirange,
};

impl<'a> Input<'a, '_> {
    pub(super) fn stored_range(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<StoredRange>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = StoredRange {
            object_id: [0; 16],
            oid: 0,
            array_oid: 0,
            array_name: String::new(),
            identity: RelationIdentity::new("", ""),
            owner: RoleIdentity::BOOTSTRAP,
            subtype: ColumnType::Integer,
            subtype_operator_class: 0,
            collation_oid: 0,
            canonical_oid: None,
            subtype_difference_oid: None,
            multirange: StoredMultirange {
                object_id: [0; 16],
                oid: 0,
                array_oid: 0,
                array_name: String::new(),
                identity: RelationIdentity::new("", ""),
            },
            usage_acl: None,
        };
        self.closed_fields(
            first,
            &[
                "object_id",
                "oid",
                "array_oid",
                "array_name",
                "identity",
                "owner",
                "subtype",
                "subtype_operator_class",
                "collation_oid",
                "canonical_oid",
                "subtype_difference_oid",
                "multirange",
                "usage_acl",
            ],
            [2559, 4095],
            |input, field, token| {
                match field {
                    0 => value.object_id = input.identity(token)?,
                    1 => value.oid = number(token)?,
                    2 => value.array_oid = number(token)?,
                    3 => value.array_name = parts.take(input.text(token)?),
                    4 => value.identity = parts.take(input.stored_relation(token)?),
                    5 => value.owner = input.stored_role_identity(token)?,
                    6 => value.subtype = parts.take(input.column(token)?),
                    7 => value.subtype_operator_class = number(token)?,
                    8 => value.collation_oid = number(token)?,
                    9 => value.canonical_oid = optional(token)?,
                    10 => value.subtype_difference_oid = optional(token)?,
                    11 => value.multirange = parts.take(input.stored_multirange(token)?),
                    12 => value.usage_acl = parts.take(input.stored_usage_acl(token)?),
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn stored_multirange(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<StoredMultirange>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = StoredMultirange {
            object_id: [0; 16],
            oid: 0,
            array_oid: 0,
            array_name: String::new(),
            identity: RelationIdentity::new("", ""),
        };
        self.closed_fields(
            first,
            &["object_id", "oid", "array_oid", "array_name", "identity"],
            [31, 31],
            |input, field, token| {
                match field {
                    0 => value.object_id = input.identity(token)?,
                    1 => value.oid = number(token)?,
                    2 => value.array_oid = number(token)?,
                    3 => value.array_name = parts.take(input.text(token)?),
                    4 => value.identity = parts.take(input.stored_relation(token)?),
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}
