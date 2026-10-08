//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_depend` and `pg_shdepend` for the user objects of one catalog snapshot, derived from their definitions as `PostgreSQL` records the rows when it creates the objects, and the descriptions `getObjectDescription` gives the objects the rows name.

mod addresses;
mod columns;
mod composite_fields;
mod composite_storage;
mod constraints;
mod defaults;
mod descriptions;
mod events;
mod expressions;
mod foreign;
mod indexes;
mod objects;
mod queries;
mod recorder;
mod references;
mod relations;
mod routines;
mod rows;
mod shared;
mod types;

use crate::catalog::context::CatalogContext;
use crate::catalog::{CatalogReadView, RelationNameResolution};
pub use addresses::CatalogObject;
use expressions::{ColumnScope, ExpressionReferences};
pub use objects::RelationKind;
use objects::{catalog_oid, CatalogObjects, ConstraintOwner, MemberObject, RelationObject};
use recorder::DependencyRecorder;
use references::References;
use uqa_sql::catalog::dependencies::{
    DependencyGraph, DependencyKind, ObjectAddress, SharedDependency, NAMESPACE_CLASS, ROLE_CLASS,
};
use uqa_sql::{ResultRow, SQLError};

/// The dependencies of one catalog snapshot, and the objects they name.
#[derive(Debug)]
pub struct CatalogDependencies {
    graph: DependencyGraph,
    shared: Vec<SharedDependency>,
    objects: CatalogObjects,
}

impl CatalogDependencies {
    /// Derive the dependencies of every user object in `catalog`, whose names `resolution` binds.
    pub fn build(
        context: &CatalogContext<'_>,
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
    ) -> Result<Self, SQLError> {
        let objects = CatalogObjects::collect(context, catalog, resolution)?;
        let mut builder = DependencyBuilder {
            context,
            catalog,
            resolution,
            objects,
            recorder: DependencyRecorder::default(),
            shared: Vec::new(),
        };
        builder.record_types()?;
        builder.record_relations()?;
        builder.record_foreign()?;
        builder.record_constraints()?;
        builder.record_defaults()?;
        builder.record_indexes()?;
        builder.record_routines()?;
        builder.record_triggers()?;
        builder.record_rules()?;
        builder.record_shared()?;
        let DependencyBuilder {
            objects,
            recorder,
            shared,
            ..
        } = builder;
        let mut edges = recorder.finish(objects.unpinned());
        // Rows are stored as their objects are created, in OID order.
        edges.sort_by_key(|edge| edge.dependent.object_id);
        Ok(Self {
            graph: DependencyGraph::new(edges),
            shared,
            objects,
        })
    }

    pub const fn graph(&self) -> &DependencyGraph {
        &self.graph
    }

    /// The object `address` names; `None` for one that does not exist or is not a user object.
    pub fn catalog_object(&self, address: ObjectAddress) -> Option<CatalogObject> {
        self.objects.catalog_object(address)
    }

    /// The address of a relation, or of one of its columns.
    pub fn relation_address(
        &self,
        identity: &uqa_core::RelationIdentity,
        column: Option<&str>,
    ) -> Option<ObjectAddress> {
        self.objects.relation_address(identity, column)
    }

    /// The address of the constraint, trigger or rule of `class_id` named `name` on the relation.
    pub fn relation_member_address(
        &self,
        class_id: u32,
        identity: &uqa_core::RelationIdentity,
        name: &str,
    ) -> Option<ObjectAddress> {
        self.objects
            .relation_member_address(class_id, identity, name)
    }

    /// The address of the default of a relation's column.
    pub fn column_default_address(
        &self,
        identity: &uqa_core::RelationIdentity,
        column: &str,
    ) -> Option<ObjectAddress> {
        self.objects.column_default_address(identity, column)
    }

    /// The address of a domain's constraint.
    pub fn domain_constraint_address(&self, domain: u32, name: &str) -> Option<ObjectAddress> {
        self.objects.domain_constraint_address(domain, name)
    }

    /// The address of a user routine.
    pub fn routine_address(&self, object_id: &[u8; 16]) -> Option<ObjectAddress> {
        self.objects
            .routine_oid(object_id)
            .map(|oid| ObjectAddress::whole(uqa_sql::catalog::dependencies::PROCEDURE_CLASS, oid))
    }

    /// The address of a schema.
    pub fn schema_address(&self, name: &str) -> Option<ObjectAddress> {
        self.objects
            .namespace_oid(name)
            .map(|oid| ObjectAddress::whole(NAMESPACE_CLASS, oid))
    }

    pub fn shared(&self) -> &[SharedDependency] {
        &self.shared
    }

    /// `getObjectDescription`: `None` for an object that does not exist.
    pub fn describe(
        &self,
        context: &CatalogContext<'_>,
        object: ObjectAddress,
    ) -> Result<Option<String>, SQLError> {
        descriptions::describe(context, &self.objects, object)
    }

    /// `checkSharedDependencies`: the detail listing what depends on the role, `None` when nothing does.
    pub fn role_dependency_detail(
        &self,
        context: &CatalogContext<'_>,
        role: u32,
    ) -> Result<Option<String>, SQLError> {
        uqa_sql::catalog::dependencies::shared_dependency_detail(
            &self.shared,
            ObjectAddress::whole(ROLE_CLASS, role),
            &|object| self.describe(context, object),
        )
    }

    /// `pg_depend` rows.
    pub fn depend_rows(&self) -> Vec<ResultRow> {
        self.graph.edges().iter().map(rows::depend_row).collect()
    }

    /// `pg_shdepend` rows.
    pub fn shared_depend_rows(&self) -> Vec<ResultRow> {
        self.shared.iter().map(rows::shared_depend_row).collect()
    }
}

/// `checkSharedDependencies` for a role, over the catalog `context` reads.
pub fn role_dependency_detail(
    context: &CatalogContext<'_>,
    role: uqa_core::catalog_role::RoleIdentity,
) -> Result<Option<String>, SQLError> {
    let catalog = context.catalog_read_view();
    let mut resolution = context.session_execution_view().relation_name_resolution();
    resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    CatalogDependencies::build(context, &catalog, &resolution)?
        .role_dependency_detail(context, catalog_oid(role.oid)?)
}

/// `pg_describe_object(classid, objid, objsubid)`: the description of an object, `NULL` for one that does not exist and for the pinned placeholder with class and object zero.
pub fn pg_describe_object_value(
    context: &CatalogContext<'_>,
    arguments: &[uqa_core::Value],
) -> Result<uqa_core::Value, SQLError> {
    use uqa_core::Value;
    let [class_id, object_id, sub_id] = arguments else {
        return Err(SQLError::BadArity {
            name: "pg_describe_object".into(),
            expected: "3".into(),
            actual: arguments.len(),
        });
    };
    let (class_id, object_id, sub_id) = match (class_id, object_id, sub_id) {
        (Value::Int(class_id), Value::Int(object_id), Value::Int(sub_id)) => {
            (*class_id, *object_id, *sub_id)
        }
        (Value::Null, _, _) | (_, Value::Null, _) | (_, _, Value::Null) => return Ok(Value::Null),
        _ => {
            return Err(SQLError::TypeMismatch(
                "pg_describe_object requires an oid, an oid and an integer".into(),
            ))
        }
    };
    if class_id == 0 && object_id == 0 {
        return Ok(Value::Null);
    }
    let object = ObjectAddress {
        class_id: catalog_oid(class_id)?,
        object_id: catalog_oid(object_id)?,
        sub_id: i32::try_from(sub_id)
            .map_err(|_| SQLError::Internal(format!("object subid {sub_id} is out of range")))?,
    };
    let dependencies = super::regtypes::catalog_dependencies(context)?;
    Ok(dependencies
        .describe(context, object)?
        .map_or(Value::Null, Value::Str))
}

/// The dependencies being derived from one catalog snapshot.
struct DependencyBuilder<'a> {
    context: &'a CatalogContext<'a>,
    catalog: &'a CatalogReadView,
    resolution: &'a RelationNameResolution,
    objects: CatalogObjects,
    recorder: DependencyRecorder,
    shared: Vec<SharedDependency>,
}

impl DependencyBuilder<'_> {
    fn expressions(&self) -> ExpressionReferences<'_> {
        ExpressionReferences {
            context: self.context,
            objects: &self.objects,
            catalog: self.catalog,
            resolution: self.resolution,
        }
    }

    /// The dependency of an object on the schema that contains it.
    fn record_namespace(&mut self, dependent: ObjectAddress, schema: &str) {
        if let Some(namespace) = self.objects.namespace_oid(schema) {
            let mut references = References::default();
            references.add(ObjectAddress::whole(NAMESPACE_CLASS, namespace));
            self.recorder
                .record_references(dependent, references, DependencyKind::Normal);
        }
    }
}
