//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::context::{RoleDependencyRead, RoleTableSecurity, RoleTablesRead};
use super::*;
use crate::{catalog::stored_view::StoredView, routines::SQLUserFunction};
use std::{cell::RefCell, ops::Deref, rc::Rc, sync::Arc};

pub(super) type Events = Rc<RefCell<Vec<String>>>;
pub(super) struct Table {
    pub name: String,
    pub security: TableSecurity,
    pub events: Events,
}
impl RoleTableSecurity for Table {
    fn security(&self) -> TableSecurity {
        self.events
            .borrow_mut()
            .push(format!("security {}", self.name));
        self.security.clone()
    }
}
struct Read<'a, T> {
    value: &'a T,
    name: &'static str,
    events: Events,
}
impl<T> Deref for Read<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value
    }
}
impl<T> Drop for Read<'_, T> {
    fn drop(&mut self) {
        self.events
            .borrow_mut()
            .push(format!("release {}", self.name));
    }
}
impl RoleTablesRead for Read<'_, BTreeMap<RelationIdentity, Table>> {
    fn iter(&self) -> Box<dyn Iterator<Item = (&RelationIdentity, &dyn RoleTableSecurity)> + '_> {
        Box::new(
            self.value
                .iter()
                .map(|(relation, table)| (relation, table as &dyn RoleTableSecurity)),
        )
    }
}
pub(super) struct Catalog {
    pub database: DatabaseSecurity,
    pub schemas: BTreeMap<String, SchemaSecurity>,
    pub tables: BTreeMap<RelationIdentity, Table>,
    pub views: BTreeMap<RelationIdentity, StoredView>,
    pub foreign_tables: BTreeMap<RelationIdentity, TableSecurity>,
    pub sequences: BTreeMap<RelationIdentity, SequenceSecurity>,
    pub routines: BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    pub events: Events,
}
impl Catalog {
    pub fn new() -> Self {
        Self {
            database: DatabaseSecurity::bootstrap(),
            schemas: BTreeMap::new(),
            tables: BTreeMap::new(),
            views: BTreeMap::new(),
            foreign_tables: BTreeMap::new(),
            sequences: BTreeMap::new(),
            routines: BTreeMap::new(),
            events: Rc::default(),
        }
    }
    fn read<'a, T>(&self, name: &'static str, value: &'a T) -> Read<'a, T> {
        self.events.borrow_mut().push(format!("read {name}"));
        Read {
            value,
            name,
            events: self.events.clone(),
        }
    }
    pub fn table(&mut self, name: &str, owner: &str) {
        self.tables.insert(
            RelationIdentity::new("public", name),
            Table {
                name: name.into(),
                security: TableSecurity::owner(owner),
                events: self.events.clone(),
            },
        );
    }
}
impl RoleDependencyCatalog for Catalog {
    fn database(&self) -> RoleDependencyRead<'_, DatabaseSecurity> {
        Box::new(self.read("database", &self.database))
    }
    fn schemas(&self) -> RoleDependencyRead<'_, BTreeMap<String, SchemaSecurity>> {
        Box::new(self.read("schemas", &self.schemas))
    }
    fn tables(&self) -> Box<dyn RoleTablesRead + '_> {
        Box::new(self.read("tables", &self.tables))
    }
    fn views(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, StoredView>> {
        Box::new(self.read("views", &self.views))
    }
    fn foreign_tables(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, TableSecurity>> {
        Box::new(self.read("foreign", &self.foreign_tables))
    }
    fn sequences(&self) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, SequenceSecurity>> {
        Box::new(self.read("sequences", &self.sequences))
    }
    fn routines(&self) -> RoleDependencyRead<'_, BTreeMap<String, Vec<Arc<SQLUserFunction>>>> {
        Box::new(self.read("routines", &self.routines))
    }
}
