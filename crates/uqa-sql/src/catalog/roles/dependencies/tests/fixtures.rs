//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::context::{RoleDependencyRead, RoleTableSecurity, RoleTablesRead};
use super::*;
use crate::catalog::security::database::BoundDatabaseSecurity;
use crate::catalog::security::schema_registry::SchemaRegistry;
use crate::catalog::security::sequence_registry::SequenceSecurities;
use crate::catalog::security::BoundTableSecurity;
use std::{cell::RefCell, ops::Deref, rc::Rc};

pub(super) type Events = Rc<RefCell<Vec<String>>>;
pub(super) struct Table {
    pub name: String,
    pub persistence: crate::ast::RelationPersistence,
    pub security: BoundTableSecurity,
    pub events: Events,
}
impl RoleTableSecurity for Table {
    fn persistence(&self) -> crate::ast::RelationPersistence {
        self.persistence
    }
    fn security(&self) -> BoundTableSecurity {
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
    pub database: BoundDatabaseSecurity,
    pub schemas: SchemaRegistry,
    pub tables: BTreeMap<RelationIdentity, Table>,
    pub views: crate::catalog::stored_view::ViewDefinitions,
    pub foreign_tables: crate::catalog::security::foreign_tables::ForeignTableSecurities,
    pub system_relations: crate::catalog::security::system_relations::SystemRelationSecurities,
    pub sequences: SequenceSecurities,
    pub routines: crate::routines::registry::RoutineRegistry,
    pub domains:
        crate::catalog::type_definitions::TypeDefinitions<crate::catalog::domain::StoredDomain>,
    pub events: Events,
}
impl Catalog {
    pub fn new() -> Self {
        Self {
            database: BoundDatabaseSecurity::bootstrap(),
            schemas: SchemaRegistry::new(),
            tables: BTreeMap::new(),
            views: crate::catalog::stored_view::ViewDefinitions::new(),
            foreign_tables:
                crate::catalog::security::foreign_tables::ForeignTableSecurities::default(),
            system_relations:
                crate::catalog::security::system_relations::SystemRelationSecurities::new(),
            sequences: SequenceSecurities::default(),
            routines: crate::routines::registry::RoutineRegistry::new(),
            domains: crate::catalog::type_definitions::TypeDefinitions::new(),
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
    pub fn table(&mut self, name: &str, owner: crate::catalog::roles::RoleIdentity) {
        self.tables.insert(
            RelationIdentity::new("public", name),
            Table {
                name: name.into(),
                persistence: crate::ast::RelationPersistence::Permanent,
                security: BoundTableSecurity::owner(owner),
                events: self.events.clone(),
            },
        );
    }
}

impl super::super::context::TemporaryRoleDependencyCatalog for Catalog {
    fn temporary_namespace_allocated(&self) -> bool {
        true
    }
}
impl RoleDependencyCatalog for Catalog {
    fn database(&self) -> RoleDependencyRead<'_, BoundDatabaseSecurity> {
        Box::new(self.read("database", &self.database))
    }
    fn schemas(&self) -> RoleDependencyRead<'_, SchemaRegistry> {
        Box::new(self.read("schemas", &self.schemas))
    }
    fn tables(&self) -> Box<dyn RoleTablesRead + '_> {
        Box::new(self.read("tables", &self.tables))
    }
    fn views(&self) -> RoleDependencyRead<'_, crate::catalog::stored_view::ViewDefinitions> {
        Box::new(self.read("views", &self.views))
    }
    fn foreign_tables(
        &self,
    ) -> RoleDependencyRead<'_, crate::catalog::security::foreign_tables::ForeignTableSecurities>
    {
        Box::new(self.read("foreign", &self.foreign_tables))
    }
    fn sequences(&self) -> RoleDependencyRead<'_, SequenceSecurities> {
        Box::new(self.read("sequences", &self.sequences))
    }
    fn routines(&self) -> RoleDependencyRead<'_, crate::routines::registry::RoutineRegistry> {
        Box::new(self.read("routines", &self.routines))
    }
    fn domains(
        &self,
    ) -> RoleDependencyRead<
        '_,
        crate::catalog::type_definitions::TypeDefinitions<crate::catalog::domain::StoredDomain>,
    > {
        Box::new(self.read("domains", &self.domains))
    }
}

impl crate::catalog::security::system_relations::SystemRelationSecurityCatalog for Catalog {
    fn system_relation_securities(
        &self,
    ) -> crate::catalog::security::system_relations::SystemRelationSecurityRead<'_> {
        Box::new(&self.system_relations)
    }
}
