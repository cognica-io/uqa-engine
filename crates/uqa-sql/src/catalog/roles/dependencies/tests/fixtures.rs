//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::context::{RoleDependencyRead, RoleTableSecurity, RoleTablesRead};
use super::*;
use crate::catalog::{roles::RoleDefinition, security::database::BoundDatabaseSecurity};
use crate::{catalog::stored_view::StoredView, routines::SQLUserFunction};
use std::{cell::RefCell, ops::Deref, rc::Rc, sync::Arc};

pub(super) type Events = Rc<RefCell<Vec<String>>>;
pub(super) struct Table {
    pub name: String,
    pub persistence: crate::ast::RelationPersistence,
    pub security: TableSecurity,
    pub events: Events,
}
impl RoleTableSecurity for Table {
    fn persistence(&self) -> crate::ast::RelationPersistence {
        self.persistence
    }
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
    pub roles: BTreeMap<String, RoleDefinition>,
    pub database: BoundDatabaseSecurity,
    pub schemas: BTreeMap<String, SchemaSecurity>,
    pub tables: BTreeMap<RelationIdentity, Table>,
    pub views: BTreeMap<RelationIdentity, StoredView>,
    pub foreign_tables: BTreeMap<RelationIdentity, TableSecurity>,
    pub system_relations: crate::catalog::security::system_relations::SystemRelationSecurities,
    pub sequences: BTreeMap<RelationIdentity, SequenceSecurity>,
    pub sequence_persistence: BTreeMap<RelationIdentity, crate::ast::RelationPersistence>,
    pub routines: BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    pub events: Events,
}
impl Catalog {
    pub fn new() -> Self {
        Self {
            roles: ["uqa", "first", "second", "unreferenced"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| {
                    let mut role = RoleDefinition::bootstrap();
                    if index != 0 {
                        role.name = name.into();
                        role.oid = 20_000 + index as i64;
                        role.object_id = [index as u8; 16];
                        role.attributes.clear();
                    }
                    (name.into(), role)
                })
                .collect(),
            database: BoundDatabaseSecurity::bootstrap(),
            schemas: BTreeMap::new(),
            tables: BTreeMap::new(),
            views: BTreeMap::new(),
            foreign_tables: BTreeMap::new(),
            system_relations: BTreeMap::new(),
            sequences: BTreeMap::new(),
            sequence_persistence: BTreeMap::new(),
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
                persistence: crate::ast::RelationPersistence::Permanent,
                security: TableSecurity::owner(owner),
                events: self.events.clone(),
            },
        );
    }
}

impl super::super::context::TemporaryRoleDependencyCatalog for Catalog {
    fn temporary_namespace_allocated(&self) -> bool {
        true
    }
    fn sequence_persistence(
        &self,
    ) -> RoleDependencyRead<'_, BTreeMap<RelationIdentity, crate::ast::RelationPersistence>> {
        Box::new(self.read("sequence persistence", &self.sequence_persistence))
    }
}
impl RoleDependencyCatalog for Catalog {
    fn database(&self) -> RoleDependencyRead<'_, BoundDatabaseSecurity> {
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

impl crate::catalog::security::system_relations::SystemRelationSecurityCatalog for Catalog {
    fn system_relation_securities(
        &self,
    ) -> crate::catalog::security::system_relations::SystemRelationSecurityRead<'_> {
        Box::new(&self.system_relations)
    }
}
