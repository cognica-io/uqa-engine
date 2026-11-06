//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned native catalog images retain identities and evaluated authority without executing SQL.

use super::{
    roles::{CapturedRoleDefinitions, CapturedRoleMemberships, RoleDefinitions, RoleMemberships},
    security::{
        database::{CapturedDatabaseAuthority, DatabaseAuthority},
        schema_registry::{CapturedSchemaRegistry, SchemaRegistry},
    },
};
use crate::SQLError;
use std::io::{Read, Write};
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod fields;
mod input;
mod reader;
pub use reader::CatalogImageReader;
use reader::Entries;

const ROLES: [u8; 8] = *b"UQAROL01";
const MEMBERSHIPS: [u8; 8] = *b"UQARMB01";
const SCHEMAS: [u8; 8] = *b"UQASCH01";
const DATABASE: [u8; 8] = *b"UQADBA01";

fn invalid(message: &'static str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: format!("invalid catalog image: {message}"),
    }
}

macro_rules! catalog_image {
    ($captured:ty, $registry:ty, $entries:ident, $magic:ident, $read:ident) => {
        impl $captured {
            /// Borrow the original captured root. Preparation does not scan or copy definitions; each read emits at most 16 KiB.
            pub fn image_reader<'a>(
                &'a self,
                cancellation: &'a CancellationToken,
            ) -> Result<CatalogImageReader<'a>, SQLError> {
                CatalogImageReader::new(
                    Entries::$entries(self.iter()),
                    $magic,
                    self.len(),
                    cancellation,
                )
            }
            /// Drain the same bounded reader. The enclosing artifact owns publication and discards failed output.
            pub fn write_image(
                &self,
                writer: &mut dyn Write,
                cancellation: &CancellationToken,
            ) -> Result<u64, SQLError> {
                self.image_reader(cancellation)?.write_to(writer)
            }
        }
        impl $registry {
            /// Decode one complete image into an unpublished native registry. Payloads and tree construction retain `memory`; logical cross-catalog closure remains the enclosing restore owner's responsibility.
            pub fn read_image(
                reader: &mut dyn Read,
                memory: &MemoryBudget,
                cancellation: &CancellationToken,
            ) -> Result<Self, SQLError> {
                input::$read(reader, memory, cancellation)
            }
        }
    };
}

catalog_image!(
    CapturedRoleDefinitions,
    RoleDefinitions,
    Roles,
    ROLES,
    roles
);
catalog_image!(
    CapturedRoleMemberships,
    RoleMemberships,
    Memberships,
    MEMBERSHIPS,
    memberships
);
catalog_image!(
    CapturedSchemaRegistry,
    SchemaRegistry,
    Schemas,
    SCHEMAS,
    schemas
);

impl CapturedDatabaseAuthority {
    pub fn image_reader<'a>(
        &'a self,
        cancellation: &'a CancellationToken,
    ) -> Result<CatalogImageReader<'a>, SQLError> {
        CatalogImageReader::new(Entries::Database(Some(self)), DATABASE, 1, cancellation)
    }
    pub fn write_image(
        &self,
        writer: &mut dyn Write,
        cancellation: &CancellationToken,
    ) -> Result<u64, SQLError> {
        self.image_reader(cancellation)?.write_to(writer)
    }
}
impl DatabaseAuthority {
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        input::database(reader, memory, cancellation)
    }
}

#[cfg(test)]
mod tests;
