//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored wrapper function bindings and server lifetimes form normal catalog dependencies.

use super::DependencyBuilder;
use uqa_sql::{
    catalog::{
        dependencies::{
            DependencyKind, ObjectAddress, FOREIGN_SERVER_CLASS, FOREIGN_WRAPPER_CLASS,
            PROCEDURE_CLASS,
        },
        foreign_wrapper::ForeignWrapperHandler,
    },
    SQLError,
};

impl DependencyBuilder<'_> {
    pub(super) fn record_foreign(&mut self) -> Result<(), SQLError> {
        let definitions = &self.catalog.snapshot().definitions;
        for wrapper in definitions.foreign_wrappers.values() {
            let handler = match &wrapper.handler {
                ForeignWrapperHandler::Function(binding) => Some(binding),
                _ => None,
            };
            for function in handler.into_iter().chain(wrapper.validator.iter()) {
                // Built-in routines are pinned; user dependencies follow the saved incarnation.
                if function.binding.builtin {
                    continue;
                }
                let address = ObjectAddress::whole(PROCEDURE_CLASS, function.oid);
                // PostgreSQL retains this edge when the validator deletes itself before wrapper publication.
                self.objects.unpin(address);
                self.recorder.record(
                    ObjectAddress::whole(FOREIGN_WRAPPER_CLASS, wrapper.identity.oid),
                    address,
                    DependencyKind::Normal,
                );
            }
        }
        for server in definitions.foreign_servers.values() {
            let reference = server.metadata.wrapper_reference.ok_or_else(|| {
                SQLError::Internal("server is missing its wrapper reference".into())
            })?;
            let address = ObjectAddress::whole(FOREIGN_WRAPPER_CLASS, reference.oid);
            if reference.oid >= uqa_sql::catalog::oids::FIRST_NORMAL_OBJECT_ID {
                self.objects.unpin(address);
            }
            self.recorder.record(
                ObjectAddress::whole(FOREIGN_SERVER_CLASS, server.metadata.oid),
                address,
                DependencyKind::Normal,
            );
        }
        Ok(())
    }
}
