//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid_grant, DefaultAclObject, SQLError};
use crate::ast::{TablePrivilege, TablePrivilegeSpec};
use serde::{Deserialize, Serialize};
use uqa_core::{
    catalog_acl::TablePrivileges, catalog_schema::SchemaPrivileges,
    catalog_sequence::SequencePrivileges,
};

/// Privilege and grant-option fields use the bit positions in `PostgreSQL`'s `acl.h`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DefaultRights(pub u16);

const INSERT: u16 = 1;
const SELECT: u16 = 1 << 1;
const UPDATE: u16 = 1 << 2;
const DELETE: u16 = 1 << 3;
const TRUNCATE: u16 = 1 << 4;
const REFERENCES: u16 = 1 << 5;
const TRIGGER: u16 = 1 << 6;
const EXECUTE: u16 = 1 << 7;
const USAGE: u16 = 1 << 8;
const CREATE: u16 = 1 << 9;
const MAINTAIN: u16 = 1 << 14;

const RIGHTS: &[(u16, &str, char)] = &[
    (INSERT, "INSERT", 'a'),
    (SELECT, "SELECT", 'r'),
    (UPDATE, "UPDATE", 'w'),
    (DELETE, "DELETE", 'd'),
    (TRUNCATE, "TRUNCATE", 'D'),
    (REFERENCES, "REFERENCES", 'x'),
    (TRIGGER, "TRIGGER", 't'),
    (EXECUTE, "EXECUTE", 'X'),
    (USAGE, "USAGE", 'U'),
    (CREATE, "CREATE", 'C'),
    (1 << 10, "TEMP", 'T'),
    (1 << 11, "CONNECT", 'c'),
    (1 << 12, "SET", 's'),
    (1 << 13, "ALTER SYSTEM", 'A'),
    (MAINTAIN, "MAINTAIN", 'm'),
];

impl DefaultRights {
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub(super) fn insert(&mut self, rights: Self) {
        self.0 |= rights.0;
    }
    pub(super) fn remove(&mut self, rights: Self) {
        self.0 &= !rights.0;
    }
    const fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }
    pub fn letters(self, options: Self) -> String {
        let mut output = String::new();
        for &(bit, _, letter) in RIGHTS {
            if self.has(bit) {
                output.push(letter);
                if options.has(bit) {
                    output.push('*');
                }
            }
        }
        output
    }
    pub(super) fn table(self) -> TablePrivileges {
        TablePrivileges {
            select: self.has(SELECT),
            insert: self.has(INSERT),
            update: self.has(UPDATE),
            delete: self.has(DELETE),
            truncate: self.has(TRUNCATE),
            references: self.has(REFERENCES),
            trigger: self.has(TRIGGER),
            maintain: self.has(MAINTAIN),
        }
    }
    pub(super) fn sequence(self) -> SequencePrivileges {
        SequencePrivileges {
            select: self.has(SELECT),
            update: self.has(UPDATE),
            usage: self.has(USAGE),
        }
    }
    pub(super) fn schema(self) -> SchemaPrivileges {
        SchemaPrivileges {
            create: self.has(CREATE),
            usage: self.has(USAGE),
        }
    }
}

impl DefaultAclObject {
    pub const fn code(self) -> char {
        match self {
            Self::Table => 'r',
            Self::Sequence => 'S',
            Self::Routine => 'f',
            Self::Type => 'T',
            Self::Schema => 'n',
            Self::LargeObject => 'L',
        }
    }
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Table => "relation",
            Self::Sequence => "sequence",
            Self::Routine => "function",
            Self::Type => "type",
            Self::Schema => "schema",
            Self::LargeObject => "large object",
        }
    }
    pub const fn rights(self) -> DefaultRights {
        DefaultRights(match self {
            Self::Table => {
                INSERT | SELECT | UPDATE | DELETE | TRUNCATE | REFERENCES | TRIGGER | MAINTAIN
            }
            Self::Sequence => SELECT | UPDATE | USAGE,
            Self::Routine => EXECUTE,
            Self::Type => USAGE,
            Self::Schema => USAGE | CREATE,
            Self::LargeObject => SELECT | UPDATE,
        })
    }
    pub fn requested(self, input: &[TablePrivilegeSpec]) -> Result<DefaultRights, SQLError> {
        if input.is_empty() {
            return Ok(self.rights());
        }
        let mut rights = DefaultRights::default();
        for item in input {
            if !item.columns.is_empty() {
                return Err(invalid_grant(
                    "default privileges cannot be set for columns",
                ));
            }
            if let TablePrivilege::Unrecognized(name) = &item.privilege {
                return Err(super::super::table::unrecognized_privilege(name));
            }
            let name = super::super::table::table_privilege_name(&item.privilege);
            let &(bit, _, _) = RIGHTS
                .iter()
                .find(|(_, privilege, _)| *privilege == name)
                .ok_or_else(|| {
                    super::super::table::unrecognized_privilege(&name.to_ascii_lowercase())
                })?;
            if !self.rights().has(bit) {
                return Err(invalid_grant(format!(
                    "invalid privilege type {name} for {}",
                    self.noun()
                )));
            }
            rights.0 |= bit;
        }
        Ok(rights)
    }
    pub fn check_namespace(self, namespace: u32) -> Result<(), SQLError> {
        let name = match self {
            Self::Schema => "SCHEMAS",
            Self::LargeObject => "LARGE OBJECTS",
            _ => return Ok(()),
        };
        if namespace != 0 {
            return Err(invalid_grant(format!(
                "cannot use IN SCHEMA clause when using GRANT/REVOKE ON {name}"
            )));
        }
        Ok(())
    }
}
