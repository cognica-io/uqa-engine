//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::{Cell, RefCell};
use uqa_sql::{
    ast::{VacuumOption, VacuumTarget},
    maintenance::{VacuumCatalog, VacuumPrivileges, VacuumRelation},
};
use uqa_storage::{StorageBackendResult, StoredDocument};

#[derive(Default)]
struct Fixture {
    calls: RefCell<Vec<&'static str>>,
    depth: Cell<usize>,
    denied: Cell<bool>,
    failure: RefCell<Option<StorageBackendError>>,
}

impl Fixture {
    fn context(&self) -> VacuumContext<'_> {
        VacuumContext {
            catalog: self,
            privileges: self,
            relations: self,
            locks: self,
            storage: self,
            rows: self,
            statistics: self,
            transactions: self,
        }
    }
    fn operation(&self, name: &'static str) -> StorageBackendResult<()> {
        self.calls.borrow_mut().push(name);
        self.failure.borrow_mut().take().map_or(Ok(()), Err)
    }
}
struct Relation;
impl VacuumRelation for Relation {
    fn column_names(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
impl VacuumCatalog for Fixture {
    fn resolve_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        Ok((name == "t").then(|| (name.into(), "table")))
    }
    fn require_table(&self, _: &str) -> Result<Box<dyn VacuumRelation + '_>, SQLError> {
        Ok(Box::new(Relation))
    }
}
impl VacuumPrivileges for Fixture {
    fn ensure_maintain(&self, _: &str) -> Result<(), SQLError> {
        if self.denied.get() {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "denied".into(),
            });
        }
        Ok(())
    }
}
impl VacuumRelations for Fixture {
    fn require_table(&self, _: &str) -> Result<Box<dyn VacuumTable + '_>, SQLError> {
        panic!("ordinary VACUUM must not rewrite tuples")
    }
    fn scan_tables(&self, _: &str, _: bool) -> Result<Vec<String>, SQLError> {
        panic!("ordinary VACUUM must not rewrite tuples")
    }
}
impl VacuumLocks for Fixture {
    fn lock_exclusive(&self, _: &str) -> Result<(), SQLError> {
        panic!("ordinary VACUUM must not acquire rewrite locks")
    }
    fn release_session(&self) {
        panic!("ordinary VACUUM must not release caller locks")
    }
}
impl VacuumStorage for Fixture {
    fn reclaim_obsolete(&self) -> StorageBackendResult<()> {
        self.operation("reclaim")
    }
    fn vacuum(&self) -> StorageBackendResult<()> {
        self.operation("compact")
    }
    fn clear_btree_indexes(&self, _: &str) -> StorageBackendResult<()> {
        panic!("ordinary VACUUM must not clear indexes")
    }
}
impl VacuumRows for Fixture {
    fn restore_document(
        &self,
        _: &str,
        _: uqa_core::DocId,
        _: StoredDocument,
        _: crate::mutation::publication::DocumentVectors,
    ) -> Result<(), SQLError> {
        panic!("ordinary VACUUM must not rewrite tuples")
    }
    fn refresh_indexes(&self, _: &str) -> StorageBackendResult<()> {
        panic!("ordinary VACUUM must not rebuild indexes")
    }
    fn note_table_data_changed(&self) {
        panic!("ordinary VACUUM must not publish data changes")
    }
}
impl VacuumStatisticsRefresh for Fixture {
    fn table_names(&self, _: &str) -> Result<Vec<String>, SQLError> {
        Ok(vec!["t".into()])
    }
    fn analyze_target(&self, _: &str, _: &[String], _: bool) -> StorageBackendResult<()> {
        self.operation("analyze")
    }
}
impl VacuumTransactions for Fixture {
    fn depth(&self) -> usize {
        self.depth.get()
    }
    fn with_maintenance_write(&self, _: VacuumWrite<'_>) -> StorageBackendResult<()> {
        panic!("ordinary VACUUM must not start a rewrite transaction")
    }
}

fn statement(options: &[&str], target: Option<&str>) -> VacuumStmt {
    VacuumStmt {
        options: options
            .iter()
            .map(|name| VacuumOption {
                name: (*name).into(),
                value: None,
            })
            .collect(),
        targets: target
            .into_iter()
            .map(|name| VacuumTarget {
                catalog: None,
                table: name.into(),
                include_descendants: true,
                columns: vec![],
            })
            .collect(),
    }
}

#[test]
fn vacuum_routes_reclamation_before_analysis_and_keeps_database_stats_separate() {
    for (options, target, expected) in [
        (vec![], None, vec!["reclaim"]),
        (vec![], Some("t"), vec!["reclaim"]),
        (vec!["analyze"], Some("t"), vec!["reclaim", "analyze"]),
        (vec!["full"], None, vec!["compact"]),
        (vec!["only_database_stats"], None, vec![]),
    ] {
        let fixture = Fixture::default();
        run_vacuum(&fixture.context(), &statement(&options, target)).unwrap();
        assert_eq!(*fixture.calls.borrow(), expected);
    }
}

#[test]
fn vacuum_validates_before_reclaiming_and_preserves_resource_failures() {
    for (options, target, depth, denied, state) in [
        (vec!["invalid"], None, 1, false, "42601"),
        (vec![], None, 1, false, "25001"),
        (
            vec!["full", "disable_page_skipping"],
            None,
            1,
            false,
            "0A000",
        ),
        (vec![], Some("missing"), 1, false, "25001"),
        (vec!["invalid"], None, 0, false, "42601"),
        (vec![], Some("missing"), 0, false, "42P01"),
        (vec![], Some("t"), 0, true, "42501"),
    ] {
        let fixture = Fixture::default();
        fixture.depth.set(depth);
        fixture.denied.set(denied);
        let error = run_vacuum(&fixture.context(), &statement(&options, target)).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state));
        assert!(fixture.calls.borrow().is_empty());
    }
    for full in [false, true] {
        for (failure, state) in [
            (StorageBackendError::from(uqa_core::QueryCancelled), "57014"),
            (
                StorageBackendError::from(uqa_core::memory::MemoryError::Limit {
                    required: 2,
                    limit: 1,
                }),
                "53200",
            ),
        ] {
            let fixture = Fixture::default();
            *fixture.failure.borrow_mut() = Some(failure);
            let options = if full {
                vec!["full", "analyze"]
            } else {
                vec!["analyze"]
            };
            let error = run_vacuum(&fixture.context(), &statement(&options, None)).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state));
            assert_eq!(
                *fixture.calls.borrow(),
                [if full { "compact" } else { "reclaim" }]
            );
        }
    }
}
