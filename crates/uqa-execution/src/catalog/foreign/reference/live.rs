//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A retired user object may stay bound across name reuse. A live incarnation must retain its original name and OID.

use super::{ForeignServerReference, StoredForeignTable};
use crate::catalog::selection_work::{checkpoint, equal, inline, NameComparison, Work};
use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::{
    catalog::{
        foreign_server::{ForeignServerDefinition, ForeignServers},
        foreign_wrapper::{ForeignWrapperReference, ForeignWrappers},
        named_definitions::NamedDefinitions,
        oids::FIRST_NORMAL_OBJECT_ID,
    },
    SQLError,
};

pub(crate) fn validate_live_servers<'a>(
    tables: impl Iterator<Item = &'a StoredForeignTable>,
    servers: &ForeignServers,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(servers_with_work(tables, servers, control, &mut ()))
}

pub(crate) async fn servers_with_work<'a>(
    tables: impl Iterator<Item = &'a StoredForeignTable>,
    servers: &ForeignServers,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
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
        |_, _| Ok(false),
        ("foreign table", "foreign server"),
        control,
        work,
    )
    .await
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
    inline(wrappers_with_work(servers, wrappers, control, &mut ()))
}

pub(crate) async fn wrappers_with_work(
    servers: &ForeignServers,
    wrappers: &ForeignWrappers,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    let bindings = servers.values().map(|server| {
        let Some(identity) = server.metadata.wrapper_reference else {
            return Err(InvalidReference::MissingWrapper(&server.name));
        };
        if identity.oid == 0 || identity.object_id == [0; 16] {
            return Err(InvalidReference::MalformedWrapper);
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
        |identity: ForeignWrapperReference, current| {
            if identity.oid >= FIRST_NORMAL_OBJECT_ID {
                return Ok(false);
            }
            identity.resolve(current)?;
            Ok(true)
        },
        ("foreign server", "foreign-data wrapper"),
        control,
        work,
    )
    .await
}

#[derive(PartialEq, Eq)]
struct Selection<'a, I> {
    name: &'a str,
    identity: I,
}

enum InvalidReference<'a> {
    MissingWrapper(&'a str),
    MalformedWrapper,
}

trait Incarnation: Copy + Eq {
    fn object_id(self) -> [u8; 16];
}

impl Incarnation for ForeignServerReference {
    fn object_id(self) -> [u8; 16] {
        self.object_id
    }
}

impl Incarnation for ForeignWrapperReference {
    fn object_id(self) -> [u8; 16] {
        self.object_id
    }
}

async fn validate_bindings<'a, I: Incarnation, T>(
    bindings: impl Iterator<Item = Result<Option<(&'a str, Selection<'a, I>)>, InvalidReference<'a>>>,
    targets: &NamedDefinitions<T>,
    identify: impl Fn(&T) -> Selection<'_, I>,
    reserved: impl Fn(I, Option<&T>) -> Result<bool, SQLError>,
    kinds: (&str, &str),
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    // Only a name miss or reuse needs a second lookup by incarnation.
    let mut unresolved = ProductionMap::new(*control);
    for binding in bindings {
        checkpoint(control, work).await?;
        let (owner, selection) = match binding {
            Ok(Some(binding)) => binding,
            Ok(None) => continue,
            Err(InvalidReference::MissingWrapper(name)) => {
                return invalid(
                    &["foreign server `", name, "` has no wrapper identity"],
                    control,
                    work,
                )
                .await;
            }
            Err(InvalidReference::MalformedWrapper) => {
                return Err(SQLError::Internal(
                    "invalid stored foreign-wrapper reference".into(),
                ));
            }
        };
        let id = selection.identity.object_id();
        let current = targets
            .get_from(
                selection.name,
                &mut NameComparison {
                    control: *control,
                    work,
                },
            )
            .await?;
        if reserved(selection.identity, current)? {
            continue;
        }
        match current.map(&identify) {
            Some(current) if id == current.identity.object_id() => {
                if selection.identity != current.identity
                    || !equal(selection.name, current.name, control, work).await?
                {
                    return inconsistent(kinds, owner, current.name, control, work).await;
                }
            }
            _ => {
                unresolved.insert_copy(id, owner)?;
            }
        }
    }
    if !unresolved.is_empty() {
        for target in targets.values() {
            checkpoint(control, work).await?;
            let current = identify(target);
            if let Some(owner) = unresolved.get(&current.identity.object_id()) {
                return inconsistent(kinds, owner, current.name, control, work).await;
            }
        }
    }
    Ok(())
}

async fn inconsistent(
    (source, target_kind): (&str, &str),
    owner: &str,
    target: &str,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    invalid(
        &[
            source,
            " `",
            owner,
            "` has an inconsistent binding to live ",
            target_kind,
            " `",
            target,
            "`",
        ],
        control,
        work,
    )
    .await
}

async fn invalid(
    parts: &[&str],
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    let mut message = uqa_core::memory::ProductionString::new(*control);
    for &part in parts {
        let mut tail = part;
        while !tail.is_empty() {
            checkpoint(control, work).await?;
            let appended = message.push_str_chunk(tail)?;
            tail = &tail[appended..];
        }
    }
    Err(SQLError::Internal(message.finish()?.into_parts().0))
}
