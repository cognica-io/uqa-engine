//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    plan::{CommandPlan, QueryPlan, RelationalPlan},
    SQLError,
};

fn command(sql: &str) -> CommandPlan {
    let UnifiedPlan::Command(command) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    *command
}

fn subqueries(command: &mut CommandPlan) -> &mut Vec<QueryPlan> {
    match command {
        CommandPlan::Insert(plan) => &mut plan.subqueries,
        CommandPlan::Update(plan) => &mut plan.subqueries,
        CommandPlan::Delete(plan) => &mut plan.subqueries,
        CommandPlan::Merge(plan) => &mut plan.subqueries,
        _ => panic!("mutation"),
    }
}

fn execute(engine: &Engine, command: &CommandPlan) -> Result<(), SQLError> {
    use uqa_execution::mutation::{dispatch, insert::table::InsertPlanning};
    let context = engine.mutation_statement_context();
    match command {
        CommandPlan::Insert(plan) => dispatch::run_insert(
            &context,
            InsertPlanning {
                inference: engine.inference_context(),
                returning: engine.returning_analysis_context(),
                prune_source_outputs:
                    uqa_planner::mutation_outputs::prune_unused_query_outputs_owned,
            },
            plan,
            &[],
            None,
        ),
        CommandPlan::Update(plan) => dispatch::run_update(&context, plan, &[], None),
        CommandPlan::Delete(plan) => dispatch::run_delete(&context, plan, &[], None),
        CommandPlan::Merge(plan) => dispatch::run_merge(&context, plan, &[], None),
        _ => panic!("mutation"),
    }
    .map(drop)
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn automatic_view_commands_admit_private_plans_before_copying(
    #[case] provider: Provider,
    #[values("insert", "update", "delete", "merge")] kind: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("automatic-view-command.db"));
    engine.sql("CREATE TABLE items(id integer PRIMARY KEY,note text); CREATE VIEW v AS SELECT id,note FROM items", &[]).unwrap();
    let sql = match kind {
        "insert" => "INSERT INTO public.v(id,note) VALUES(1,'x') RETURNING note",
        "update" => "UPDATE public.v SET note='x' WHERE id=1 RETURNING note",
        "delete" => "DELETE FROM public.v WHERE note='x' RETURNING note",
        "merge" => "MERGE INTO public.v AS target USING (VALUES(1,'x')) AS source(id,note) ON target.id=source.id WHEN MATCHED THEN UPDATE SET note=source.note WHEN NOT MATCHED THEN INSERT(id,note) VALUES(source.id,source.note) RETURNING target.note",
        _ => unreachable!(),
    };
    // Mutation entry resolves relation names before invoking this dispatch boundary.
    let mut plan = command(sql);
    let UnifiedPlan::Query(mut unused) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT 1").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut unused.root else {
        panic!("block")
    };
    block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    subqueries(&mut plan).push(*unused);
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(execute(&engine, &plan));
    });
    let output = output.unwrap();
    assert!(
        matches!(&output, Err(error) if error.sqlstate() == Some("53200")),
        "{provider:?}/{kind}: expected admission rejection, got {output:?}; {measured:?}"
    );
    assert!(
        measured.bytes_total < 256 << 10,
        "{provider:?}/{kind}: {measured:?}"
    );
    drop(output);
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    engine.sql(sql, &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
