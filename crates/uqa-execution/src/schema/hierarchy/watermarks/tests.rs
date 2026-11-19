//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    ast::{AutoIncrement, PartitionBound, PartitionSpec, PartitionStrategy},
    catalog::table_definition::TableDefinition,
};

#[derive(Clone)]
struct Table(Arc<State>);

struct State {
    columns: SharedTableDefinition<Vec<ColumnDef>>,
    hierarchy: SharedTableDefinition<TableHierarchy>,
    watermark: Mutex<u128>,
    fail_read: bool,
}

impl WatermarkTable for Table {
    fn columns(&self) -> SharedTableDefinition<Vec<ColumnDef>> {
        self.0.columns.clone()
    }
    fn hierarchy(&self) -> SharedTableDefinition<TableHierarchy> {
        self.0.hierarchy.clone()
    }
    fn watermark(&self) -> StorageBackendResult<u128> {
        if self.0.fail_read {
            return Err(StorageBackendError::Other("counter read failed".into()));
        }
        Ok(*self.0.watermark.lock())
    }
    fn raise_watermark(&self, minimum: u128) {
        let mut value = self.0.watermark.lock();
        *value = (*value).max(minimum);
    }
}

#[derive(Clone, Default)]
struct Tables(Arc<BTreeMap<RelationIdentity, Table>>);

impl WatermarkTables for Tables {
    type Table = Table;
    fn tables(&self) -> impl Iterator<Item = (&RelationIdentity, Table)> + Send {
        self.0.iter().map(|(name, table)| (name, table.clone()))
    }
}

impl Tables {
    fn insert(
        &mut self,
        name: &str,
        parent: Option<&str>,
        partitioned: bool,
        legacy: bool,
        value: u128,
    ) {
        let uqa_sql::Statement::CreateTable(table) = uqa_sql::compile("CREATE TABLE t(id bigint)")
            .unwrap()
            .remove(0)
        else {
            panic!("table")
        };
        let mut columns = table.columns;
        if legacy {
            columns[0].auto_increment = Some(AutoIncrement::legacy());
        }
        let hierarchy = TableHierarchy {
            parents: parent.into_iter().map(str::to_owned).collect(),
            partition_bound: parent.map(|_| PartitionBound::Default),
            partition_spec: partitioned.then(|| PartitionSpec {
                strategy: PartitionStrategy::Range,
                keys: Vec::new(),
            }),
            ..Default::default()
        };
        Arc::make_mut(&mut self.0).insert(
            RelationIdentity::from_legacy_name(name).unwrap(),
            Table(Arc::new(State {
                columns: TableDefinition::new(&columns).unwrap().shared(),
                hierarchy: TableDefinition::new(&hierarchy).unwrap().shared(),
                watermark: Mutex::new(value),
                fail_read: false,
            })),
        );
    }

    fn table(&self, name: &str) -> &Table {
        &self.0[&RelationIdentity::from_legacy_name(name).unwrap()]
    }
}

fn fixture() -> Tables {
    let mut tables = Tables::default();
    tables.insert("public.root", None, true, true, 2);
    tables.insert("public.middle", Some("public.root"), true, true, 30);
    tables.insert("public.leaf", Some("public.middle"), false, true, 90);
    tables.insert("public.unrelated", None, false, true, 999);
    tables.insert("public.modern", Some("public.root"), false, false, 9999);
    tables
}

fn finish(
    input: &mut WatermarkInput<Table>,
    window: usize,
) -> StorageBackendResult<(WatermarkPlan<Table>, usize)> {
    for step in 1..65536 {
        if let Some(plan) = input.advance(window)? {
            return Ok((plan, step));
        }
    }
    panic!("partition watermarks stopped progressing")
}

fn updates(plan: WatermarkPlan<Table>, tables: &Tables) -> Vec<(String, u128)> {
    plan.map(|update| {
        let name = tables
            .0
            .iter()
            .find(|(_, table)| Arc::ptr_eq(&table.0, &update.table.0))
            .unwrap()
            .0
            .qualified_name();
        (name, update.minimum)
    })
    .collect()
}

#[test]
fn partition_watermarks_preserve_legacy_roots_and_defer_monotone_publication() {
    let tables = fixture();
    assert_eq!(
        updates(plan(&tables).unwrap(), &tables),
        [("public.root".to_owned(), 90)]
    );
    for window in [1, 3, 256] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
        let (plan, steps) = finish(&mut input, window).unwrap();
        if window == 1 {
            assert!(steps > 20);
        }
        assert_eq!(tables.table("public.root").watermark().unwrap(), 2);
        assert_eq!(updates(plan, &tables), [("public.root".to_owned(), 90)]);
        assert!(input.advance(1).is_err());
        drop(input);
        assert_eq!(control.memory().used(), 0);
    }
    let mut plan = plan(&tables).unwrap();
    tables.table("public.root").raise_watermark(120);
    plan.next().unwrap().publish();
    assert!(plan.next().is_none());
    assert_eq!(tables.table("public.root").watermark().unwrap(), 120);
    assert_eq!(tables.table("public.middle").watermark().unwrap(), 30);
    assert_eq!(tables.table("public.leaf").watermark().unwrap(), 90);
}

#[test]
fn partition_watermarks_keep_quoted_name_order_and_native_definition_owners() {
    let mut tables = fixture();
    // Tuple order places `a` before `a b`; rendered qualified-name order does the reverse.
    tables.insert("a.root", None, true, true, 10);
    tables.insert("\"a b\".root", None, true, true, 20);
    let control = StorageReadControl::with_limit(1 << 20);
    let original = tables.table("public.root").columns();
    assert!(original.shares_definition(&tables.table("public.root").columns()));
    let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
    assert!(input.advance(1).unwrap().is_none());
    // Membership publication after construction cannot join the retained input.
    tables.insert("a.later", None, true, true, 100);
    let (plan, _) = finish(&mut input, 1).unwrap();
    assert_eq!(
        updates(plan, &tables),
        [
            ("\"a b\".root".into(), 20),
            ("a.root".into(), 10),
            ("public.root".into(), 90)
        ]
    );
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn partition_watermarks_reject_corrupt_ancestry_before_any_update() {
    for (parent, expected) in [
        (
            "public.absent",
            "partition hierarchy references missing table `public.absent`",
        ),
        (
            "public.zz_bad",
            "partition hierarchy cycle reaches `public.zz_bad`",
        ),
    ] {
        let mut tables = fixture();
        tables.insert("public.zz_bad", Some(parent), false, true, 1000);
        let Err(error) = plan(&tables) else {
            panic!("corrupt ancestry accepted")
        };
        assert_eq!(error.to_string(), expected);
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
        let Err(error) = finish(&mut input, 1) else {
            panic!("corrupt ancestry accepted")
        };
        assert_eq!(error.to_string(), expected);
        assert_eq!(tables.table("public.root").watermark().unwrap(), 2);
        assert!(input.advance(1).is_err());
        drop(input);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn partition_watermarks_retire_all_cancelled_denied_and_abandoned_prefixes() {
    let tables = fixture();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
    let (plan, steps) = finish(&mut input, 1).unwrap();
    let peak = control.memory().peak();
    assert!(control.memory().used() > 0);
    drop((plan, input));
    assert_eq!(control.memory().used(), 0);
    for prefix in 0..steps - 1 {
        for terminal in ["cancel", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
            for _ in 0..prefix {
                assert!(input.advance(1).unwrap().is_none());
            }
            match terminal {
                "cancel" => {
                    control.cancellation().cancel();
                    assert!(input.advance(1).is_err());
                }
                "zero" => {
                    assert!(input.advance(0).is_err());
                }
                "drop" => (),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("retire watermark input");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                _ => unreachable!(),
            }
            if matches!(terminal, "cancel" | "zero") {
                assert!(input.advance(1).is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0, "{terminal}: {prefix}");
        }
    }
    let mut admitted = 0;
    let mut denied = 0;
    for limit in (0..peak).step_by(31).chain([peak]) {
        let control = StorageReadControl::with_limit(limit);
        let result = WatermarkInput::new(tables.clone(), &control)
            .and_then(|mut input| finish(&mut input, 1));
        match result {
            Ok((plan, _)) => {
                assert_eq!(updates(plan, &tables), [("public.root".into(), 90)]);
                admitted += 1;
            }
            Err(error) => {
                assert!(error.to_string().contains("memory"), "{error}");
                denied += 1;
            }
        }
        assert!(control.memory().peak() <= limit);
        assert_eq!(control.memory().used(), 0);
    }
    assert!(admitted > 0 && denied > 0);
    assert_eq!(tables.table("public.root").watermark().unwrap(), 2);
}

#[test]
fn partition_watermarks_bound_wide_columns_and_retained_name_production() {
    let mut tables = Tables::default();
    tables.insert("public.root", None, true, true, 91);
    let state =
        Arc::get_mut(&mut Arc::make_mut(&mut tables.0).values_mut().next().unwrap().0).unwrap();
    let mut columns = vec![state.columns[0].clone(); 512];
    for (index, column) in columns.iter_mut().enumerate() {
        column.name = format!("column_{index}_{}", "x".repeat(512));
        column.auto_increment = None;
    }
    columns.last_mut().unwrap().auto_increment = Some(AutoIncrement::legacy());
    state.columns = TableDefinition::new(&columns).unwrap().shared();
    drop(columns);
    let control = StorageReadControl::with_limit(16 << 10);
    let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
    let (plan, steps) = finish(&mut input, 1).unwrap();
    assert!(steps > 512 && steps < 600, "{steps}");
    assert_eq!(updates(plan, &tables), [("public.root".into(), 91)]);
    drop(input);
    assert_eq!(control.memory().used(), 0);
    let table = tables.table("public.root").clone();
    let relation = RelationIdentity::new("public", "quoted\"한글".repeat(4096));
    let expected = relation.qualified_name();
    tables = Tables(Arc::new(BTreeMap::from([(relation, table)])));
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
    let (plan, steps) = finish(&mut input, 1).unwrap();
    // Rendering visits each 1,024-character name segment twice, in addition to the 512 column visits.
    let name_steps = 2 * ("quoted\"한글".chars().count() * 4096).div_ceil(1024);
    assert!(
        steps >= 512 + name_steps && steps < 512 + name_steps + 20,
        "{steps}"
    );
    assert_eq!(updates(plan, &tables), [(expected, 91)]);
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn partition_watermarks_keep_empty_selection_allocation_free_and_reject_parentless_partitions() {
    let mut tables = Tables::default();
    let measured = allocation_counter::measure(|| assert_eq!(plan(&tables).unwrap().count(), 0));
    assert_eq!(measured.bytes_total, 0);
    tables.insert("public.bad", None, false, true, 1);
    let state =
        Arc::get_mut(&mut Arc::make_mut(&mut tables.0).values_mut().next().unwrap().0).unwrap();
    state.hierarchy = TableDefinition::new(&TableHierarchy {
        partition_bound: Some(PartitionBound::Default),
        ..Default::default()
    })
    .unwrap()
    .shared();
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = WatermarkInput::new(tables, &control).unwrap();
    let Err(error) = finish(&mut input, 1) else {
        panic!("parentless partition accepted")
    };
    assert_eq!(error.to_string(), "partition has no parent relation");
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn partition_watermarks_discard_earlier_roots_after_a_counter_read_failure() {
    let mut tables = fixture();
    tables.insert("public.zz_failed", Some("public.root"), false, true, 2000);
    let state = Arc::make_mut(&mut tables.0)
        .get_mut(&RelationIdentity::new("public", "zz_failed"))
        .unwrap();
    Arc::get_mut(&mut state.0).unwrap().fail_read = true;
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = WatermarkInput::new(tables.clone(), &control).unwrap();
    let Err(error) = finish(&mut input, 1) else {
        panic!("failed counter read accepted")
    };
    assert_eq!(error.to_string(), "counter read failed");
    assert_eq!(tables.table("public.root").watermark().unwrap(), 2);
    assert!(input.advance(1).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
}
