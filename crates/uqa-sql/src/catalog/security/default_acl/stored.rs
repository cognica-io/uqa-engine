//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DefaultAclKey, DefaultAclObject, DefaultAclRow, DefaultAcls, DefaultRights};
use crate::{
    ast::stored_input::{number, Input, InputError, Parts},
    catalog::roles::{identity::RoleSubject, RoleDefinitions},
    SQLError,
};
use uqa_core::{
    catalog_role::{BoundAclEntry, RoleIdentity},
    json::JsonToken,
    memory::{Budgeted, Produced, ProductionControl},
};

impl DefaultAclObject {
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            b'r' => Some(Self::Table),
            b'S' => Some(Self::Sequence),
            b'f' => Some(Self::Routine),
            b'T' => Some(Self::Type),
            b'n' => Some(Self::Schema),
            b'L' => Some(Self::LargeObject),
            _ => None,
        }
    }
}

impl DefaultAclRow {
    pub(crate) fn validate_header(&self, key: DefaultAclKey) -> Result<(), SQLError> {
        if self.oid < 16_384
            || !key.owner.is_valid()
            || key.object.check_namespace(key.namespace).is_err()
            || self.acl == key.baseline()
        {
            return Err(SQLError::Internal("invalid default ACL tuple".into()));
        }
        Ok(())
    }
}

impl DefaultAclKey {
    pub(crate) fn validate_entry(
        self,
        entry: &BoundAclEntry<DefaultRights>,
        previous: Option<&BoundAclEntry<DefaultRights>>,
    ) -> Result<(), SQLError> {
        if previous.is_some_and(|prior| prior.role >= entry.role)
            || entry.grantor != self.owner
            || !entry.role.is_none_or(RoleIdentity::is_valid)
            || entry.privileges.is_empty()
            || entry.privileges.0 & !self.object.rights().0 != 0
            || entry.grant_options.0 & !entry.privileges.0 != 0
            || (entry.role.is_none() && !entry.grant_options.is_empty())
        {
            return Err(SQLError::Internal("invalid default ACL grant".into()));
        }
        Ok(())
    }
}
impl DefaultAcls {
    /// The metadata key contains the numeric endpoints. The tuple carries the full role incarnations and is admitted before publication.
    pub fn read_stored(
        &mut self,
        text: &str,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
    ) -> Result<DefaultAclKey, SQLError> {
        let mut input = Input::new(text, *control);
        let first = input.next()?;
        let mut parts = Parts::new(*control);
        let (mut owner, mut namespace, mut code, mut oid, mut acl) =
            (RoleIdentity::BOOTSTRAP, 0, 0u8, 0, Vec::new());
        input.closed_fields(
            first,
            &["owner", "namespace", "object", "oid", "acl"],
            [31, 31],
            |input, pos, token| {
                match pos {
                    0 => owner = input.stored_role_identity(token)?,
                    1 => namespace = number(token)?,
                    2 => code = number(token)?,
                    3 => oid = number(token)?,
                    _ => acl = parts.take(input.array(token, Input::default_acl_grant)?),
                }
                Ok(())
            },
        )?;
        input.end()?;
        let key = DefaultAclKey {
            owner,
            namespace,
            object: DefaultAclObject::from_code(code)
                .ok_or_else(|| SQLError::Internal("invalid default ACL object class".into()))?,
        };
        let row = DefaultAclRow {
            oid,
            temporary: false,
            acl,
        };
        row.validate_header(key)?;
        let check_role = |role: RoleIdentity| {
            role.role_name(roles).map(|_| ()).ok_or_else(|| {
                SQLError::Internal("default ACL references an unknown role incarnation".into())
            })
        };
        check_role(owner)?;
        let mut previous = None;
        for entry in &row.acl {
            control.check()?;
            key.validate_entry(entry, previous)?;
            if let Some(role) = entry.role {
                check_role(role)?;
            }
            previous = Some(entry);
        }
        if self.contains_key(&key) {
            return Err(SQLError::Internal("duplicate default ACL key".into()));
        }
        let row = parts.finish(row)?.into_budgeted().map_err(|_| {
            SQLError::Internal("default ACL input requires an admitted control".into())
        })?;
        self.insert_image_entry(Budgeted::new(key, self.budget().empty_reservation()), row)?;
        Ok(key)
    }
}

impl<'a> Input<'a, '_> {
    fn default_acl_grant(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<BoundAclEntry<DefaultRights>>, InputError> {
        let (mut role, mut grantor, mut rights, mut options) =
            (None, RoleIdentity::BOOTSTRAP, 0, 0);
        self.closed_fields(
            first,
            &["role", "grantor", "privileges", "grant_options"],
            [15, 15],
            |input, pos, token| {
                match pos {
                    0 if token != JsonToken::Null => {
                        role = Some(input.stored_role_identity(token)?)
                    }
                    0 => (),
                    1 => grantor = input.stored_role_identity(token)?,
                    2 => rights = number(token)?,
                    _ => options = number(token)?,
                }
                Ok(())
            },
        )?;
        Ok(self.control.finish(
            BoundAclEntry {
                role,
                grantor,
                privileges: DefaultRights(rights),
                grant_options: DefaultRights(options),
            },
            self.control.empty_reservation(),
        )?)
    }
}
