//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine and type privileges share their stored bound endpoint fields.

use super::{boolean, Input, InputError, JsonToken};
use crate::ast::ObjectAclEntry;
use uqa_core::{catalog_role::RoleIdentity, memory::Produced};

impl<'a> Input<'a, '_> {
    pub(crate) fn stored_object_acl(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<ObjectAclEntry>, InputError> {
        let (mut role, mut grantor, mut grant_option) = (None, RoleIdentity::BOOTSTRAP, false);
        self.fields(
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
        Ok(self.control.finish(
            ObjectAclEntry {
                role,
                grantor,
                grant_option,
            },
            self.control.empty_reservation(),
        )?)
    }
}
