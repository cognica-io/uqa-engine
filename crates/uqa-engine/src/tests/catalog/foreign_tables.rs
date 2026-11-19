//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;

mod definitions;
mod runtime;
mod server_deletion;
mod servers;
mod wrappers;
use std::{
    cell::Cell,
    ops::{Deref, DerefMut},
};
use uqa_execution::schema::{
    foreign_creation::{
        ForeignCreationRegistry, ForeignSecurityRead, ForeignServersRead, ForeignServersWrite,
        ForeignTablesRead,
    },
    publication::dependencies::CatalogPublicationChanges,
};

struct ServerWrite<'a> {
    guard: Option<
        uqa_storage::memory_capture::MemoryCaptureWriteGuard<
            'a,
            uqa_sql::catalog::foreign_server::ForeignServers,
        >,
    >,
    held: &'a Cell<bool>,
}
impl Deref for ServerWrite<'_> {
    type Target = uqa_sql::catalog::foreign_server::ForeignServers;
    fn deref(&self) -> &Self::Target {
        self.guard.as_deref().unwrap()
    }
}
impl DerefMut for ServerWrite<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.as_deref_mut().unwrap()
    }
}
impl Drop for ServerWrite<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.held.set(false);
    }
}
struct Publication<'a> {
    engine: &'a Engine,
    held: Cell<bool>,
    changes: Cell<usize>,
}
impl ForeignRegistryReads for Publication<'_> {
    fn relations(&self) -> uqa_execution::catalog::foreign::reads::ForeignCatalogRead<'_> {
        Box::new(self.engine.durable.foreign_catalog.read())
    }
    fn wrappers(&self) -> uqa_execution::catalog::foreign::reads::ForeignWrappersRead<'_> {
        Box::new(self.engine.durable.foreign_wrappers.read())
    }
    fn servers(&self) -> ForeignServersRead<'_> {
        Box::new(self.engine.durable.foreign_servers.read())
    }
    fn tables(&self) -> ForeignTablesRead<'_> {
        Box::new(self.engine.durable.foreign_catalog.tables())
    }
    fn security(&self) -> ForeignSecurityRead<'_> {
        Box::new(self.engine.durable.foreign_catalog.security())
    }
}
impl ForeignCreationRegistry for Publication<'_> {
    fn wrappers_write(&self) -> uqa_execution::schema::foreign_creation::ForeignWrappersWrite<'_> {
        Box::new(self.engine.durable.foreign_wrappers.write())
    }
    fn servers_write(&self) -> ForeignServersWrite<'_> {
        let guard = self.engine.durable.foreign_servers.write();
        assert!(!self.held.replace(true));
        Box::new(ServerWrite {
            guard: Some(guard),
            held: &self.held,
        })
    }
}
impl CatalogPublicationChanges for Publication<'_> {
    fn table_catalog_changed(&self) {
        panic!("server registration must not publish a table epoch")
    }
    fn catalog_registry_changed(&self) {
        assert!(
            !self.held.get(),
            "server write guard must be released before epoch publication"
        );
        assert!(self
            .engine
            .durable
            .foreign_servers
            .read()
            .contains_key("source"));
        self.changes.set(self.changes.get() + 1);
        self.engine.note_catalog_registry_changed();
    }
}

#[test]
fn server_registration_releases_the_actual_guard_before_epoch_publication() {
    let engine = Engine::new();
    engine.sql("BEGIN", &[]).unwrap();
    let publication = Publication {
        engine: &engine,
        held: Cell::new(false),
        changes: Cell::new(0),
    };
    let mut context = engine.foreign_creation_context();
    context.registry = &publication;
    context.changes = &publication;
    context
        .register_foreign_server_inner(
            "source".into(),
            "memory_fdw",
            vec![
                ("source".into(), "first".into()),
                ("source".into(), "last".into()),
            ],
            false,
        )
        .unwrap();
    assert_eq!(publication.changes.get(), 1);
    assert!(!publication.held.get());
    assert_eq!(
        engine
            .foreign_server_definition("source")
            .unwrap()
            .unwrap()
            .options["source"],
        "last"
    );
    context
        .register_foreign_server_inner("source".into(), "missing_fdw", Vec::new(), true)
        .unwrap();
    assert_eq!(publication.changes.get(), 1);
    assert!(!publication.held.get());
    assert_eq!(
        engine
            .foreign_server_definition("source")
            .unwrap()
            .unwrap()
            .fdw_type,
        "memory_fdw"
    );
    engine.sql("COMMIT", &[]).unwrap();
}

#[test]
fn missing_foreign_server_rolls_back_implicit_sequence_creation_before_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign.db");
    let engine = Engine::open(&path).unwrap();
    let error = engine
        .sql(
            "CREATE FOREIGN TABLE rejected(id serial) SERVER missing_server",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert!(engine.durable.foreign_catalog.tables().is_empty());
    assert!(engine.durable.foreign_catalog.security().is_empty());
    assert!(engine.durable.sequences.read().is_empty());
    let catalog = engine.storage.catalog.as_ref().unwrap();
    assert!(catalog.load_foreign_tables().unwrap().is_empty());
    assert_eq!(catalog.load_sequence_rows().unwrap().len(), 0);
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert!(reopened.durable.foreign_catalog.tables().is_empty());
    assert!(reopened.durable.sequences.read().is_empty());
}

use uqa_execution::catalog::foreign::reads::ForeignRegistryReads;

fn bounded_catalog(engine: &Engine) -> uqa_core::memory::MemoryBudget {
    let memory = uqa_core::memory::MemoryBudget::new(1 << 20);
    let current = engine.durable.foreign_catalog.read().clone();
    let mut next = uqa_execution::catalog::foreign::ForeignCatalog::with_budget(&memory);
    for (relation, table) in current.tables() {
        next.insert(
            relation.clone(),
            table.clone(),
            current.security()[relation].clone(),
        )
        .unwrap();
    }
    *engine.durable.foreign_catalog.write() = next;
    memory
}
