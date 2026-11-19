//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored role fields retain their original representation independently of catalog validation.

use super::{identity::RoleBinding, RoleDefinition, RoleMembership};
use crate::{
    ast::stored_input::{boolean, number, unit, Input, InputError, Parts},
    SQLError,
};
use uqa_core::{
    json::JsonToken,
    memory::{OwnedSet, Produced, ProductionControl},
};

impl RoleDefinition {
    /// Decode the existing role record with admitted name and attribute nodes. Identity, OID, tuple-revision and membership validation remain at the catalog boundary.
    pub fn from_stored_json_with_control(
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let mut input = Input::new(text, *control);
        let first = input.next()?;
        let value = input.role_definition(first)?;
        input.end()?;
        Ok(value)
    }
}

impl RoleMembership {
    /// Preserve stored role bindings without resolving their names or replacing their incarnations.
    pub fn from_stored_json_with_control(
        text: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let mut input = Input::new(text, *control);
        let first = input.next()?;
        let value = input.role_membership(first)?;
        input.end()?;
        Ok(value)
    }
}

impl<'a> Input<'a, '_> {
    fn role_definition(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<RoleDefinition>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = RoleDefinition {
            oid: 0,
            object_id: [0; 16],
            revision: 0,
            name: String::new(),
            attributes: OwnedSet::new(),
            connection_limit: 0,
        };
        self.fields(
            first,
            &[
                "oid",
                "object_id",
                "revision",
                "name",
                "attributes",
                "connection_limit",
            ],
            57,
            |input, field, token| {
                match field {
                    0 => value.oid = number(token)?,
                    1 => value.object_id = input.identity(token)?,
                    2 => value.revision = number(token)?,
                    3 => value.name = parts.take(input.text(token)?),
                    4 => {
                        value.attributes = parts
                            .take(input.set(token, |input, token| unit(&input.unit_tag(token)?))?);
                    }
                    _ => value.connection_limit = number(token)?,
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn role_binding(&mut self, first: JsonToken<'a>) -> Result<Produced<RoleBinding>, InputError> {
        let mut parts = Parts::new(self.control);
        let (mut name, mut oid, mut object_id) = (String::new(), 0, [0; 16]);
        self.fields(
            first,
            &["name", "oid", "object_id"],
            7,
            |input, field, token| {
                match field {
                    0 => name = parts.take(input.text(token)?),
                    1 => oid = number(token)?,
                    _ => object_id = input.identity(token)?,
                }
                Ok(())
            },
        )?;
        parts.finish(RoleBinding::new(name, oid, object_id))
    }

    fn role_membership(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<RoleMembership>, InputError> {
        let mut parts = Parts::new(self.control);
        let empty = || RoleBinding::new(String::new(), 0, [0; 16]);
        let mut value = RoleMembership {
            oid: 0,
            role: empty(),
            member: empty(),
            grantor: empty(),
            admin_option: false,
            inherit_option: false,
            set_option: false,
        };
        self.fields(
            first,
            &[
                "oid",
                "role",
                "member",
                "grantor",
                "admin_option",
                "inherit_option",
                "set_option",
            ],
            127,
            |input, field, token| {
                match field {
                    0 => value.oid = number(token)?,
                    1 => value.role = parts.take(input.role_binding(token)?),
                    2 => value.member = parts.take(input.role_binding(token)?),
                    3 => value.grantor = parts.take(input.role_binding(token)?),
                    4 => value.admin_option = boolean(token)?,
                    5 => value.inherit_option = boolean(token)?,
                    _ => value.set_option = boolean(token)?,
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}

#[cfg(test)]
mod tests;
