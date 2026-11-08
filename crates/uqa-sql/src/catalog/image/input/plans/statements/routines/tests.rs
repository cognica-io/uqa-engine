//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Statement;
use uqa_core::memory::ProductionControl;
mod footprints;
mod resources;

fn routine(sql: &str, id: u8) -> SQLRoutine {
    let Statement::CreateFunction(mut definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("routine")
    };
    definition.name = "public.f".into();
    definition.object_id = Some([id; 16]);
    definition.catalog_revision = Some([id + 40; 16]);
    definition.catalog_oid = Some(40_000 + u32::from(id));
    definition.body_origin = None;
    definition.config_actions.clear();
    definition.attribute_clauses = crate::ast::RoutineAttributeClauses::default();
    definition.owner = Some(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP);
    let body = match &definition.body {
        FunctionBody::Source(_) => RoutineBody::Source,
        FunctionBody::Statements(statements) => RoutineBody::Bound(
            CompiledFunctionBody::SQL(statements.iter().cloned().map(UnifiedPlan::lower).collect())
                .shared(),
        ),
    };
    SQLUserFunction::new(*definition, body).shared()
}

fn fixture() -> RoutineRegistry {
    RoutineRegistry::try_from_iter([("public.f".into(), vec![
        routine("CREATE FUNCTION f(a integer DEFAULT 7) RETURNS integer LANGUAGE SQL IMMUTABLE STRICT SECURITY DEFINER PARALLEL SAFE COST 42 RETURN a + 1", 1),
        routine("CREATE FUNCTION f(a text) RETURNS text LANGUAGE SQL AS 'source is preserved, not parsed'", 2),
        routine("CREATE PROCEDURE f() LANGUAGE SQL BEGIN ATOMIC INSERT INTO docs VALUES (1); UPDATE docs SET x=2; DELETE FROM docs; END", 3),
        routine("CREATE FUNCTION f(VARIADIC a integer[]) RETURNS SETOF integer LANGUAGE SQL AS 'SELECT unnest(a)'", 4),
        routine("CREATE FUNCTION f(IN a integer, OUT x text, INOUT y integer) LANGUAGE SQL AS 'SELECT a::text, y'", 5),
        routine("CREATE FUNCTION f() RETURNS TABLE (x integer) LANGUAGE SQL BEGIN ATOMIC SELECT 1; END", 6),
    ])]).unwrap()
}

fn encode(registry: &RoutineRegistry) -> Vec<u8> {
    let memory = MemoryBudget::new(1 << 25);
    let token = CancellationToken::new();
    let capture = registry
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    drain(capture.image_reader(&memory, &token).unwrap())
}

fn drain(mut reader: crate::catalog::image::DefinitionImageReader<'_>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0; 31];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
    }
    output
}

#[test]
fn native_routine_images_preserve_source_bound_bodies_and_original_authority() {
    let source = fixture();
    let bytes = encode(&source);
    let memory = MemoryBudget::new(1 << 25);
    let token = CancellationToken::new();
    let restored = RoutineRegistry::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(encode(&restored), bytes);
    for (original, decoded) in source["public.f"].iter().zip(&restored["public.f"]) {
        assert_eq!(
            serde_json::to_value(&original.def).unwrap(),
            serde_json::to_value(&decoded.def).unwrap()
        );
        match (&original.body, &decoded.body) {
            (RoutineBody::Source, RoutineBody::Source) => {}
            (RoutineBody::Bound(original), RoutineBody::Bound(decoded)) => {
                let (CompiledFunctionBody::SQL(original), CompiledFunctionBody::SQL(decoded)) =
                    (&***original, &***decoded)
                else {
                    panic!("SQL bodies")
                };
                assert_eq!(
                    serde_json::to_value(original).unwrap(),
                    serde_json::to_value(decoded).unwrap()
                );
            }
            _ => panic!("body contract changed"),
        }
    }
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_images_reject_every_truncated_prefix_without_retained_payloads() {
    let bytes = encode(&fixture());
    let token = CancellationToken::new();
    for end in 0..bytes.len() {
        let memory = MemoryBudget::new(1 << 25);
        assert!(
            RoutineRegistry::read_image(&mut &bytes[..end], &memory, &token).is_err(),
            "prefix {end}"
        );
        assert_eq!(memory.used(), 0, "prefix {end}");
    }
    for limit in [0, 1, 128, 4096, 16_384, 32_768] {
        let memory = MemoryBudget::new(limit);
        drop(RoutineRegistry::read_image(
            &mut bytes.as_slice(),
            &memory,
            &token,
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_routine_body_image_outlives_its_catalog_and_definition() {
    let bytes = encode(&fixture());
    let memory = MemoryBudget::new(1 << 25);
    let token = CancellationToken::new();
    let restored = RoutineRegistry::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    let RoutineBody::Bound(body) = &restored["public.f"][0].body else {
        panic!("bound body")
    };
    let body = body.clone();
    drop(restored);
    assert!(memory.used() > 0);
    assert!(body.is_retained_by(&memory));
    std::thread::spawn(move || {
        let CompiledFunctionBody::SQL(plans) = &**body else {
            panic!("bound SQL")
        };
        assert_eq!(plans.len(), 1);
        let scratch = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let result = drain(plans[0].image_reader(&scratch, &token).unwrap());
        assert_eq!(&result[..8], b"UQAPLN01");
        let remaining = memory.reserve(memory.limit() - memory.used()).unwrap();
        let allocations = allocation_counter::measure(|| drop(body));
        assert_eq!(allocations.count_total, 0);
        drop(remaining);
        assert_eq!(memory.used(), 0);
    })
    .join()
    .unwrap();
}

#[test]
fn native_routine_image_output_borrows_large_source_and_has_terminal_failure() {
    let original = routine(
        "CREATE FUNCTION f() RETURNS text LANGUAGE SQL AS 'unexamined'",
        1,
    );
    let mut definition = original.def.clone();
    definition.body = FunctionBody::Source("x".repeat(128 << 10));
    let registry = RoutineRegistry::try_from_iter([(
        "public.f".into(),
        vec![SQLUserFunction::new(definition, RoutineBody::Source).shared()],
    )])
    .unwrap();
    let expected = encode(&registry);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let captured = registry
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    drop(registry);
    let scratch = MemoryBudget::new(16 << 10);
    let mut reader = None;
    assert_eq!(
        allocation_counter::measure(|| {
            reader = Some(captured.image_reader(&scratch, &token).unwrap());
        })
        .count_total,
        0
    );
    let mut reader = reader.unwrap();
    let mut position = 0;
    let mut buffer = [0; 131];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        assert_eq!(&buffer[..count], &expected[position..position + count]);
        position += count;
    }
    assert_eq!(position, expected.len());
    assert_eq!(scratch.used(), 0);
    let denied = MemoryBudget::new(0);
    let mut failed = captured.image_reader(&denied, &token).unwrap();
    assert!(failed.read(&mut buffer).is_err());
    assert!(failed.read(&mut buffer).is_err());
    assert_eq!(denied.used(), 0);
    let cancelled = CancellationToken::new();
    let mut failed = captured.image_reader(&scratch, &cancelled).unwrap();
    cancelled.cancel();
    assert!(failed.read(&mut buffer).is_err());
    assert!(failed.read(&mut buffer).is_err());
    assert_eq!(scratch.used(), 0);
}
