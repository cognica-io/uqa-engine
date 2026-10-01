//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `performDeletion` for the statements that drop catalog objects: the named objects and everything that depends on them are found in the catalog's dependencies, reported as `PostgreSQL` reports them, and removed one by one with every dependent before the object it depends on.

mod labels;
mod locking;
mod plan;
mod removal;

pub use labels::drop_graph_label_dependents;

use crate::catalog::context::CatalogContext;
use crate::catalog::projection::CatalogDependencies;
use crate::row_locks::binding::RelationDefinitionSession;
use crate::schema::domains::dependencies::DomainDependencyContext;
use crate::schema::foreign_removal::ForeignTableRemovalContext;
use crate::schema::indexes::removal::IndexRemovalContext;
use crate::schema::namespaces::removal::EmptySchemaRemovalContext;
use crate::schema::table_removal::context::TableRemovalContext;
use plan::DeletionPlan;
use uqa_sql::catalog::dependencies::{DeletionTargets, ObjectAddress};
use uqa_sql::{SQLError, SQLNotice};

/// The catalog services that remove each kind of object.
pub struct CatalogRemovalContext<'a> {
    pub catalog: CatalogContext<'a>,
    pub locks: &'a dyn RelationDefinitionSession,
    /// The object identities of relations, which the deletion locks by.
    pub identities: &'a dyn crate::row_locks::binding::RelationLockCatalog,
    /// Tables, and through it views, sequences, routines, triggers, rules, columns, defaults and constraints.
    pub tables: TableRemovalContext<'a>,
    pub foreign_tables: ForeignTableRemovalContext<'a>,
    pub indexes: IndexRemovalContext<'a>,
    pub domains: DomainDependencyContext<'a>,
    /// Attributes of standalone composite types, whose removal rewrites the stored values of the type.
    pub composites: crate::schema::composites::attributes::CompositeAttributeContext<'a>,
    pub schemas: EmptySchemaRemovalContext<'a>,
    pub events: &'a dyn crate::schema::removal::RelationRemovalEvents,
    pub notices: &'a parking_lot::Mutex<Vec<SQLNotice>>,
}

/// Build the removal services lazily, so a statement's own context does not contain them.
pub trait CatalogRemovalInputs {
    fn catalog_removal_context(&self) -> CatalogRemovalContext<'_>;
}

/// `performMultipleDeletions`: remove the objects `originals` names in the catalog's dependencies and what depends on them. Without `cascade`, objects that depend on them through normal dependencies make the drop fail with `PostgreSQL`'s detail and hint; with it, they are removed too and reported in a notice. The relations to remove are locked first, and the search repeats until the locks cover every relation it finds.
pub fn perform_deletion(
    context: &CatalogRemovalContext<'_>,
    originals: impl Fn(&CatalogDependencies) -> Result<Vec<ObjectAddress>, SQLError>,
    cascade: bool,
) -> Result<(), SQLError> {
    delete_objects(context, originals, cascade, false)
}

/// `PERFORM_DELETION_QUIETLY`: a cascading deletion, such as `ON COMMIT DROP`'s, that reports nothing.
pub fn perform_quiet_cascade(
    context: &CatalogRemovalContext<'_>,
    originals: impl Fn(&CatalogDependencies) -> Result<Vec<ObjectAddress>, SQLError>,
) -> Result<(), SQLError> {
    delete_objects(context, originals, true, true)
}

fn delete_objects(
    context: &CatalogRemovalContext<'_>,
    originals: impl Fn(&CatalogDependencies) -> Result<Vec<ObjectAddress>, SQLError>,
    cascade: bool,
    quiet: bool,
) -> Result<(), SQLError> {
    let mut locked = std::collections::BTreeSet::new();
    loop {
        let dependencies = catalog_dependencies(&context.catalog)?;
        let originals = originals(&dependencies)?;
        if originals.is_empty() {
            return Ok(());
        }
        let original = match originals.as_slice() {
            [original] => Some(*original),
            _ => None,
        };
        let describe = |object| dependencies.describe(&context.catalog, object);
        let targets = DeletionTargets::collect(dependencies.graph(), &originals, &describe)?;
        let plan = DeletionPlan::new(&dependencies, &targets)?;
        if locking::lock_relations(context, &plan, &mut locked)? {
            // Waiting for a lock may have let other sessions change what depends on the objects.
            continue;
        }
        let notice = targets.report(cascade, original, &describe)?;
        plan.execute(context)?;
        if let Some(notice) = notice.filter(|_| !quiet) {
            context
                .notices
                .lock()
                .push(SQLNotice::notice(notice.message).with_detail(notice.detail));
        }
        return Ok(());
    }
}

/// `PreCommit_on_commit_actions` for `ON COMMIT DROP`: the temporary table and what depends on it, without a notice.
pub fn drop_table_on_commit(
    context: &CatalogRemovalContext<'_>,
    name: &str,
) -> Result<(), SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
    perform_quiet_cascade(context, |dependencies| {
        Ok(vec![required_address(
            dependencies.relation_address(&relation, None),
            || format!("table {name}"),
        )?])
    })
}

/// The dependencies of the catalog as other sessions last committed it, with relation names bound as the catalog stores them: `findDependentObjects` scans `pg_depend` with a fresh catalog snapshot, so a search after a lock wait sees the definitions changed while it waited, not a query's retained snapshot.
pub fn catalog_dependencies(context: &CatalogContext<'_>) -> Result<CatalogDependencies, SQLError> {
    context.catalog.refreshed_catalog_snapshot()?;
    let catalog = context.catalog.current_catalog_snapshot();
    let mut resolution = context.session_execution_view().relation_name_resolution();
    resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    CatalogDependencies::build(context, &catalog, &resolution)
}

/// The address of an object a statement names, which the dependencies must know.
pub fn required_address(
    address: Option<ObjectAddress>,
    object: impl FnOnce() -> String,
) -> Result<ObjectAddress, SQLError> {
    address.ok_or_else(|| {
        SQLError::Internal(format!(
            "{} is missing from the catalog dependencies",
            object()
        ))
    })
}
