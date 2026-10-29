//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{
    AccessPathPlan, CommandAssignmentSlot, CommandQuerySlot, CommandScalarSlot, ComputePlan,
    MutableExecutablePlan,
};

fn exercise(
    sql: &str,
    rewrite: impl FnOnce(
        MutableExecutablePlan,
        &ProductionControl<'_>,
    ) -> Result<MutableExecutablePlan, SQLError>,
) {
    let source = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(rewrite(source.copy_for_rewrite(&control).unwrap(), &control).unwrap());
    });
    let output = output.unwrap();
    assert_eq!(
        allocations.bytes_current as usize,
        output.reserved_bytes(),
        "{sql}"
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    let immutable = output.finish(&control).unwrap();
    assert_eq!(budget.used(), immutable.reserved_bytes());
    drop(immutable);
    assert_eq!(budget.used(), 0);
}

#[test]
fn query_metadata_frame_slots_and_parent_observations_retain_exact_owners() {
    exercise("SELECT sum(a) OVER w AS original FROM t WINDOW w AS (ORDER BY a ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)", |plan, control| {
        let MutableExecutablePlan::Query(query) = plan else { panic!("query"); };
        let query = query.set_compute(ComputePlan::Window, control)?.set_access(AccessPathPlan::Row, control)?;
        let query = query.map_scalar(QueryScalarSlot::WindowFrameStart(0), control, Ok)?
            .map_scalar(QueryScalarSlot::WindowFrameEnd(0), control, Ok)?;
        let query = query.map_scalar_with_parent(QueryScalarSlot::Projection(0), control, |parent, expression| {
            assert!(matches!(&parent.root, RelationalPlan::QueryBlock(block) if matches!(&block.projections[0].expr, ScalarExpr::Literal(Value::Null))));
            Ok(expression)
        })?;
        let query = query.replace_projection_alias(0, Some(control.copy_text("preserved_output")?), control)?;
        let identity = crate::ColumnIdentity::from_parts_with_control(Some("t"), "a", control)?;
        let query = query.include_privilege_column(identity, control)?;
        let duplicate = crate::ColumnIdentity::from_parts_with_control(Some("t"), "a", control)?;
        Ok(MutableExecutablePlan::Query(query.include_privilege_column(duplicate, control)?))
    });
}

#[test]
fn query_and_command_ctes_transfer_their_exact_body_kind() {
    exercise(
        "WITH changed AS (UPDATE t SET a=1 RETURNING a) SELECT * FROM changed",
        |plan, control| {
            let MutableExecutablePlan::Query(query) = plan else {
                panic!("query");
            };
            Ok(MutableExecutablePlan::Query(query.map_cte(
                0,
                control,
                |body| {
                    let MutableExecutablePlan::Command(command) = body else {
                        panic!("command CTE");
                    };
                    Ok(MutableExecutablePlan::Command(command.map_assignment(
                        CommandAssignmentSlot::Update(0),
                        control,
                        Ok,
                    )?))
                },
            )?))
        },
    );
    exercise(
        "WITH q AS (SELECT 1 a) INSERT INTO t SELECT a FROM q RETURNING (SELECT 2)",
        |plan, control| {
            let MutableExecutablePlan::Command(command) = plan else {
                panic!("command");
            };
            let command = command
                .map_cte(0, control, |body| {
                    let MutableExecutablePlan::Query(query) = body else {
                        panic!("query CTE");
                    };
                    Ok(MutableExecutablePlan::Query(query.map_scalar(
                        QueryScalarSlot::Projection(0),
                        control,
                        Ok,
                    )?))
                })?
                .map_query_child(CommandQuerySlot::InsertSource, control, Ok)?
                .map_query_child(CommandQuerySlot::Subquery(0), control, Ok)?;
            Ok(MutableExecutablePlan::Command(command))
        },
    );
}

#[test]
fn assignment_bounds_conflict_actions_and_merge_clauses_keep_order_and_leases() {
    for (sql, slot, count) in [
        ("UPDATE t SET a[1:2] = ARRAY[3]", CommandAssignmentSlot::Update(0), 3),
        ("INSERT INTO t(a) VALUES(1) ON CONFLICT(a) DO UPDATE SET a[1]=2 WHERE excluded.a>0", CommandAssignmentSlot::Conflict(0), 2),
        ("MERGE INTO t USING s ON t.a=s.a WHEN MATCHED AND t.a>0 THEN UPDATE SET a[1:2]=ARRAY[3] WHEN NOT MATCHED AND s.a>0 THEN INSERT(a) VALUES(s.a)", CommandAssignmentSlot::Merge { clause: 0, assignment: 0 }, 3),
    ] {
        exercise(sql, |plan, control| {
            let MutableExecutablePlan::Command(command) = plan else { panic!("command"); };
            let mut visited = 0;
            let command = command.map_assignment(slot, control, |expression| { visited += 1; Ok(expression) })?;
            assert_eq!(visited, count);
            let command = match slot {
                CommandAssignmentSlot::Update(_) => command,
                CommandAssignmentSlot::Conflict(_) => command.map_scalar(CommandScalarSlot::ConflictPredicate, control, Ok)?,
                CommandAssignmentSlot::Merge { .. } => command.map_scalar(CommandScalarSlot::MergeCondition(0), control, Ok)?
                    .map_scalar(CommandScalarSlot::MergeCondition(1), control, Ok)?
                    .map_scalar(CommandScalarSlot::MergeValue { clause: 1, value: 0 }, control, Ok)?,
            };
            Ok(MutableExecutablePlan::Command(command))
        });
    }
}

#[test]
fn command_sources_and_argument_query_arenas_have_independent_rewrite_owners() {
    exercise(
        "UPDATE t SET a=1 FROM (SELECT 2 a) s WHERE t.a=s.a",
        |plan, control| {
            let MutableExecutablePlan::Command(command) = plan else {
                panic!("command");
            };
            Ok(MutableExecutablePlan::Command(
                command.map_source_with_parent(control, |parent, source| {
                    assert!(matches!(parent, crate::plan::CommandPlan::Update(_)));
                    source.map_query_with_parent(control, |source, query| {
                        assert!(matches!(source, crate::plan::SourcePlan::Subquery { .. }));
                        query.map_scalar(QueryScalarSlot::Projection(0), control, Ok)
                    })
                })?,
            ))
        },
    );
    exercise("CALL f((SELECT 1))", |plan, control| {
        let MutableExecutablePlan::Command(command) = plan else {
            panic!("command");
        };
        Ok(MutableExecutablePlan::Command(command.map_query_child(
            CommandQuerySlot::CallArgumentSubquery {
                argument: 0,
                query: 0,
            },
            control,
            Ok,
        )?))
    });
}

#[test]
fn selecting_one_query_scalar_retires_every_other_constructor() {
    let source = fixture();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut selected = None;
    let allocations = allocation_counter::measure(|| {
        selected = source
            .copy_for_rewrite(&control)
            .unwrap()
            .select_scalar(QueryScalarSlot::Projection(0), &control)
            .unwrap();
    });
    let selected = selected.unwrap();
    assert_eq!(
        allocations.bytes_current as usize,
        selected.reserved_bytes()
    );
    assert_eq!(budget.used(), selected.reserved_bytes());
    drop(selected);
    assert_eq!(budget.used(), 0);
}
