//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invalidate selected routine identities after a successful catalog write, including identical-content tuple replacement.

use crate::schema::namespaces::NamespaceCatalogChanges;
use crate::statement::prepared::invalidation::PreparedCatalogChange;
use std::collections::BTreeMap;
use uqa_sql::routines::lifecycle::RoutineRegistry;

pub(in crate::routines) fn record_changes(
    changes: &dyn NamespaceCatalogChanges,
    before: &RoutineRegistry,
    after: &RoutineRegistry,
) {
    let mut previous = before
        .values()
        .flatten()
        .filter_map(|function| {
            function
                .def
                .object_id
                .map(|identity| (identity, function.catalog_revision()))
        })
        .collect::<BTreeMap<_, _>>();
    for function in after.values().flatten() {
        if let Some(identity) = function.def.object_id {
            if previous.remove(&identity) != Some(function.catalog_revision()) {
                changes.prepared_catalog_changed(PreparedCatalogChange::Routine(identity));
            }
        }
    }
    for identity in previous.into_keys() {
        changes.prepared_catalog_changed(PreparedCatalogChange::Routine(identity));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, sync::Arc};
    use uqa_sql::routines::{RoutineBody, SQLUserFunction};

    struct Changes(RefCell<Vec<PreparedCatalogChange>>);
    impl NamespaceCatalogChanges for Changes {
        fn catalog_registry_changed(&self) {
            panic!("publication boundary is retained by caller");
        }
        fn prepared_catalog_changed(&self, change: PreparedCatalogChange) {
            self.0.borrow_mut().push(change);
        }
    }

    #[test]
    fn exact_routine_tuple_replacements_additions_and_removals_do_not_touch_other_overloads() {
        let uqa_sql::Statement::CreateFunction(mut definition) =
            uqa_sql::compile("CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'")
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        definition.object_id = Some([1; 16]);
        let first =
            super::super::revision::replacement(*definition.clone(), RoutineBody::Source).unwrap();
        definition.object_id = Some([2; 16]);
        let unchanged = Arc::new(SQLUserFunction::new(*definition, RoutineBody::Source));
        let before = BTreeMap::from([("public.f".into(), vec![first.clone(), unchanged.clone()])]);
        let changes = Changes(RefCell::new(vec![]));
        record_changes(&changes, &before, &before);
        assert!(changes.0.borrow().is_empty());
        let replaced =
            super::super::revision::replacement(first.def.clone(), RoutineBody::Source).unwrap();
        let after = BTreeMap::from([("public.f".into(), vec![replaced, unchanged])]);
        record_changes(&changes, &before, &after);
        assert_eq!(
            *changes.0.borrow(),
            [PreparedCatalogChange::Routine([1; 16])]
        );
        changes.0.borrow_mut().clear();
        record_changes(&changes, &BTreeMap::new(), &after);
        assert_eq!(
            *changes.0.borrow(),
            [
                PreparedCatalogChange::Routine([1; 16]),
                PreparedCatalogChange::Routine([2; 16])
            ]
        );
        changes.0.borrow_mut().clear();
        record_changes(&changes, &after, &BTreeMap::new());
        assert_eq!(
            *changes.0.borrow(),
            [
                PreparedCatalogChange::Routine([1; 16]),
                PreparedCatalogChange::Routine([2; 16])
            ]
        );
    }
}
