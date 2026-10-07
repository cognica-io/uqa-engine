//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign catalog deletion shares lifetime binding while SQL owns each object's diagnostics.

use super::{ForeignCatalogRemovalContext, SQLError};
use uqa_sql::catalog::{
    dependencies::{ObjectAddress, FOREIGN_SERVER_CLASS, FOREIGN_WRAPPER_CLASS},
    foreign_server::ForeignServerDefinition,
    foreign_wrapper::ForeignWrapperDefinition,
};

#[derive(Clone, Copy)]
pub(super) enum ForeignKind {
    Server,
    Wrapper,
}

pub(super) enum ForeignTarget {
    Server(ForeignServerDefinition),
    Wrapper(ForeignWrapperDefinition),
}

impl ForeignKind {
    pub fn lookup(
        self,
        context: &ForeignCatalogRemovalContext<'_>,
        name: &str,
    ) -> Option<ForeignTarget> {
        let registry = context.publication.registry;
        match self {
            Self::Server => registry
                .servers()
                .get(name)
                .cloned()
                .map(ForeignTarget::Server),
            Self::Wrapper => registry
                .wrappers()
                .get(name)
                .cloned()
                .map(ForeignTarget::Wrapper),
        }
    }

    pub fn missing(self, name: &str) -> SQLError {
        match self {
            Self::Server => uqa_sql::schema::foreign_servers::missing_server(name),
            Self::Wrapper => uqa_sql::schema::foreign_wrappers::missing_wrapper(name),
        }
    }

    pub fn notice(self, name: &str) -> uqa_sql::SQLNotice {
        match self {
            Self::Server => uqa_sql::schema::foreign_servers::missing_server_notice(name),
            Self::Wrapper => uqa_sql::schema::foreign_wrappers::missing_wrapper_notice(name),
        }
    }
}

impl ForeignTarget {
    pub fn address(&self) -> ObjectAddress {
        match self {
            Self::Server(server) => ObjectAddress::whole(FOREIGN_SERVER_CLASS, server.metadata.oid),
            Self::Wrapper(wrapper) => {
                ObjectAddress::whole(FOREIGN_WRAPPER_CLASS, wrapper.identity.oid)
            }
        }
    }

    pub fn incarnation(&self) -> [u8; 16] {
        match self {
            Self::Server(server) => server.metadata.object_id,
            Self::Wrapper(wrapper) => wrapper.identity.object_id,
        }
    }

    pub fn ensure_authority(
        &self,
        context: &ForeignCatalogRemovalContext<'_>,
    ) -> Result<(), SQLError> {
        match self {
            Self::Server(server) => uqa_sql::schema::foreign_servers::ensure_drop_authority(
                server,
                context.session,
                context.roles,
            ),
            Self::Wrapper(wrapper) => uqa_sql::schema::foreign_wrappers::ensure_drop_authority(
                wrapper,
                context.session,
                context.roles,
            ),
        }
    }
}
