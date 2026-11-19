//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    boolean, number, Input, InputError, JsonToken, Parts, Produced, RelationIdentity, RoleIdentity,
    StoredComposite,
};
use crate::{
    ast::{stored_input::optional, ColumnType},
    catalog::composite_type::StoredCompositeAttribute,
};

impl<'a> Input<'a, '_> {
    pub(super) fn stored_composite(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<StoredComposite>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = StoredComposite {
            object_id: [0; 16],
            oid: 0,
            relation_oid: 0,
            array_oid: 0,
            array_name: String::new(),
            identity: RelationIdentity::new("", ""),
            owner: RoleIdentity::BOOTSTRAP,
            attributes: Vec::new(),
            usage_acl: None,
        };
        self.closed_fields(
            first,
            &[
                "object_id",
                "oid",
                "relation_oid",
                "array_oid",
                "array_name",
                "identity",
                "owner",
                "attributes",
                "usage_acl",
            ],
            [255, 255],
            |input, field, token| {
                match field {
                    0 => value.object_id = input.identity(token)?,
                    1 => value.oid = number(token)?,
                    2 => value.relation_oid = number(token)?,
                    3 => value.array_oid = number(token)?,
                    4 => value.array_name = parts.take(input.text(token)?),
                    5 => value.identity = parts.take(input.stored_relation(token)?),
                    6 => value.owner = input.stored_role_identity(token)?,
                    7 => value.attributes = parts.take(input.array(token, Self::stored_attribute)?),
                    8 => value.usage_acl = parts.take(input.stored_usage_acl(token)?),
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn stored_attribute(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<StoredCompositeAttribute>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = StoredCompositeAttribute {
            name: String::new(),
            ty: ColumnType::Integer,
            collation: None,
            collation_oid: None,
            number: 0,
            dropped: false,
        };
        self.closed_fields(
            first,
            &[
                "name",
                "ty",
                "collation",
                "collation_oid",
                "number",
                "dropped",
            ],
            [19, 19],
            |input, field, token| {
                match field {
                    0 => value.name = parts.take(input.text(token)?),
                    1 => value.ty = parts.take(input.column(token)?),
                    2 => {
                        value.collation =
                            parts.take(input.maybe(token, |input, token| input.text(token))?);
                    }
                    3 => value.collation_oid = optional(token)?,
                    4 => value.number = number(token)?,
                    5 => value.dropped = boolean(token)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}
