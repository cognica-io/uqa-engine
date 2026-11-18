//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A retired user object may stay bound across name reuse. A live incarnation must retain its original name and OID.

use super::{ForeignServerReference, StoredForeignTable};
use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::{
    catalog::{
        foreign_server::{ForeignServerDefinition, ForeignServers},
        foreign_wrapper::ForeignWrappers,
        named_definitions::NamedDefinitionView,
        oids::FIRST_NORMAL_OBJECT_ID,
    },
    SQLError,
};

pub(crate) fn validate_live_servers<'a>(
    tables: impl Iterator<Item = &'a StoredForeignTable>,
    servers: &ForeignServers,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let bindings = tables.map(|table| {
        Ok(table
            .selected_server()
            .map(|selection| (table.name.as_str(), selection)))
    });
    validate_bindings(
        bindings,
        servers,
        server_selection,
        |reference| reference.object_id,
        ("foreign table", "foreign server"),
        control,
    )
}

impl StoredForeignTable {
    fn selected_server(&self) -> Option<Selection<'_, ForeignServerReference>> {
        self.server_reference.map(|identity| Selection {
            name: &self.server_name,
            identity,
        })
    }

    pub(super) fn selects_server(&self, server: &ForeignServerDefinition) -> bool {
        self.selected_server() == Some(server_selection(server))
    }
}

fn server_selection(server: &ForeignServerDefinition) -> Selection<'_, ForeignServerReference> {
    Selection {
        name: &server.name,
        identity: ForeignServerReference::from(server),
    }
}

pub(crate) fn validate_live_wrappers(
    servers: &ForeignServers,
    wrappers: &ForeignWrappers,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let bindings = servers.values().map(|server| {
        let Some(identity) = server.metadata.wrapper_reference else {
            let message = control.format(format_args!(
                "foreign server `{}` has no wrapper identity",
                server.name
            ))?;
            return Err(SQLError::Internal(message.into_parts().0));
        };
        if identity.oid == 0 || identity.object_id == [0; 16] {
            return Err(SQLError::Internal(
                "invalid stored foreign-wrapper reference".into(),
            ));
        }
        if identity.oid < FIRST_NORMAL_OBJECT_ID {
            // Reserved host implementations cannot be deleted by SQL, including inside a validator.
            server.bound_wrapper(wrappers)?;
            return Ok(None);
        }
        Ok(Some((
            server.name.as_str(),
            Selection {
                name: &server.fdw_type,
                identity,
            },
        )))
    });
    validate_bindings(
        bindings,
        wrappers,
        |wrapper| Selection {
            name: &wrapper.name,
            identity: wrapper.identity,
        },
        |reference| reference.object_id,
        ("foreign server", "foreign-data wrapper"),
        control,
    )
}

#[derive(PartialEq, Eq)]
struct Selection<'a, I> {
    name: &'a str,
    identity: I,
}

fn validate_bindings<'a, I: Copy + Eq, T>(
    bindings: impl Iterator<Item = Result<Option<(&'a str, Selection<'a, I>)>, SQLError>>,
    targets: &impl NamedDefinitionView<T>,
    identify: impl Fn(&T) -> Selection<'_, I>,
    object_id: impl Fn(I) -> [u8; 16],
    kinds: (&str, &str),
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let inconsistent = |owner: &str, target: &str| {
        let (source_kind, target_kind) = kinds;
        let message = control.format(format_args!(
            "{source_kind} `{owner}` has an inconsistent binding to live {target_kind} `{target}`"
        ))?;
        Err(SQLError::Internal(message.into_parts().0))
    };
    // Only a name miss or reuse needs a second lookup by incarnation.
    let mut unresolved = ProductionMap::new(*control);
    for binding in bindings {
        control.check()?;
        let Some((owner, selection)) = binding? else {
            continue;
        };
        let id = object_id(selection.identity);
        match targets.get(selection.name).map(&identify) {
            Some(current) if id == object_id(current.identity) => {
                if selection != current {
                    return inconsistent(owner, current.name);
                }
            }
            _ => {
                unresolved.insert_copy(id, owner)?;
            }
        }
    }
    if !unresolved.is_empty() {
        for target in targets.values() {
            control.check()?;
            let current = identify(target);
            if let Some(owner) = unresolved.get(&object_id(current.identity)) {
                return inconsistent(owner, current.name);
            }
        }
    }
    Ok(())
}
