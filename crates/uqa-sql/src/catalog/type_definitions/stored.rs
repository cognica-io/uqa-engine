//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored type declarations retain original field buffers before registry-wide validation.

use crate::{
    ast::{
        stored_input::{boolean, invalid, number, Input, InputError, Parts},
        ObjectAclEntry,
    },
    catalog::{
        composite_type::StoredComposite,
        enum_type::{StoredEnum, StoredEnumLabel},
        range_type::StoredRange,
        shell_type::StoredShellType,
    },
    SQLError,
};
use uqa_core::{
    catalog_role::RoleIdentity,
    json::JsonToken,
    memory::{Produced, ProductionControl},
    EnumLabelKey, RelationIdentity, MAX_ENUM_LABEL_KEY_BYTES,
};

macro_rules! stored {
    ($type:ty, $read:ident) => {
        impl $type {
            /// Decode one physically bounded stored record under its original production allowance. Complete identity, claim and owner validation stays at the registry boundary.
            pub fn from_stored_json_with_control(
                text: &str,
                control: &ProductionControl<'_>,
            ) -> Result<Produced<Self>, SQLError> {
                let mut input = Input::new(text, *control);
                let first = input.next()?;
                let value = input.$read(first)?;
                input.end()?;
                Ok(value)
            }
        }
    };
}

stored!(StoredEnum, stored_enum);
stored!(StoredShellType, stored_shell);
stored!(StoredRange, stored_range);
stored!(StoredComposite, stored_composite);

mod composite;
mod domain;
mod range;

impl<'a> Input<'a, '_> {
    fn stored_enum(&mut self, first: JsonToken<'a>) -> Result<Produced<StoredEnum>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = StoredEnum {
            object_id: [0; 16],
            oid: 0,
            array_oid: 0,
            array_name: String::new(),
            identity: RelationIdentity::new("", ""),
            owner: RoleIdentity::BOOTSTRAP,
            labels: Vec::new(),
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
                "labels",
                "usage_acl",
            ],
            [127, 127],
            |input, field, token| {
                match field {
                    0 => value.object_id = input.identity(token)?,
                    1 => value.oid = number(token)?,
                    2 => value.array_oid = number(token)?,
                    3 => value.array_name = parts.take(input.text(token)?),
                    4 => value.identity = parts.take(input.stored_relation(token)?),
                    5 => value.owner = input.stored_role_identity(token)?,
                    6 => value.labels = parts.take(input.array(token, Self::stored_enum_label)?),
                    7 => value.usage_acl = parts.take(input.stored_usage_acl(token)?),
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn stored_shell(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<StoredShellType>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = StoredShellType {
            object_id: [0; 16],
            oid: 0,
            identity: RelationIdentity::new("", ""),
            owner: RoleIdentity::BOOTSTRAP,
            usage_acl: None,
        };
        self.closed_fields(
            first,
            &["object_id", "oid", "identity", "owner", "usage_acl"],
            [15, 15],
            |input, field, token| {
                match field {
                    0 => value.object_id = input.identity(token)?,
                    1 => value.oid = number(token)?,
                    2 => value.identity = parts.take(input.stored_relation(token)?),
                    3 => value.owner = input.stored_role_identity(token)?,
                    4 => value.usage_acl = parts.take(input.stored_usage_acl(token)?),
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn stored_enum_label(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<StoredEnumLabel>, InputError> {
        let mut parts = Parts::new(self.control);
        let (mut oid, mut key, mut label, mut sort_order) = (0, None, String::new(), 0.0);
        self.closed_fields(
            first,
            &["oid", "key", "label", "sort_order"],
            [15, 15],
            |input, field, token| {
                match field {
                    0 => oid = number(token)?,
                    1 => key = Some(parts.take(input.stored_enum_key(token)?)),
                    2 => label = parts.take(input.text(token)?),
                    3 => {
                        let JsonToken::Number(text) = token else {
                            return Err(invalid());
                        };
                        // Typed JSON conversion preserves Serde's float4 rounding and signed zero. The enclosing physical record bounds this scalar conversion; it does not materialize a generic arbitrary-precision number.
                        input.control.check()?;
                        sort_order = serde_json::from_str::<f32>(text).map_err(|_| invalid())?;
                        input.control.check()?;
                    }
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        let key = key.ok_or_else(invalid)?;
        parts.finish(StoredEnumLabel {
            oid,
            key,
            label,
            sort_order,
        })
    }

    fn stored_enum_key(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<EnumLabelKey>, InputError> {
        let text = self.text(token)?;
        let bytes = text.len() / 2;
        if !text.len().is_multiple_of(2) || bytes > MAX_ENUM_LABEL_KEY_BYTES {
            return Err(invalid());
        }
        // Core validates hexadecimal digits and the immutable-key invariant. Admission covers both its vector and possible compact boxed replacement until construction finishes.
        let mut memory = self.control.reserve(bytes * 2)?;
        let key = EnumLabelKey::from_hex(&text).map_err(|_| invalid())?;
        if let Some(memory) = memory.as_mut() {
            drop(memory.split(bytes));
        }
        Ok(self.control.finish(key, memory)?)
    }

    fn stored_relation(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<RelationIdentity>, InputError> {
        let mut parts = Parts::new(self.control);
        let (mut schema, mut name) = (String::new(), String::new());
        self.fields(first, &["schema", "name"], 3, |input, field, token| {
            if field == 0 {
                schema = parts.take(input.text(token)?);
            } else {
                name = parts.take(input.text(token)?);
            }
            Ok(())
        })?;
        parts.finish(RelationIdentity { schema, name })
    }

    fn stored_role_identity(&mut self, first: JsonToken<'a>) -> Result<RoleIdentity, InputError> {
        let (mut oid, mut object_id) = (0, [0; 16]);
        self.fields(first, &["oid", "object_id"], 3, |input, field, token| {
            if field == 0 {
                oid = number(token)?;
            } else {
                object_id = input.identity(token)?;
            }
            Ok(())
        })?;
        Ok(RoleIdentity { oid, object_id })
    }

    fn stored_usage_acl(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<Option<Vec<ObjectAclEntry>>>, InputError> {
        self.maybe(first, |input, first| {
            input.array(first, |input, first| {
                let (mut role, mut grantor, mut grant_option) =
                    (None, RoleIdentity::BOOTSTRAP, false);
                input.fields(
                    first,
                    &["role", "grantor", "grant_option"],
                    7,
                    |input, field, token| {
                        match field {
                            0 if token != JsonToken::Null => {
                                role = Some(input.stored_role_identity(token)?);
                            }
                            1 => grantor = input.stored_role_identity(token)?,
                            2 => grant_option = boolean(token)?,
                            _ => (),
                        }
                        Ok(())
                    },
                )?;
                Ok(input.control.finish(
                    ObjectAclEntry {
                        role,
                        grantor,
                        grant_option,
                    },
                    input.control.empty_reservation(),
                )?)
            })
        })
    }
}

#[cfg(test)]
mod tests;
