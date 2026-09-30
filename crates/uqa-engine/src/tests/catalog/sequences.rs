//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

mod alteration_authority;
mod dependencies;
mod lifecycle;
mod ownership;
mod privileges;
mod removal;
mod restoration;
mod values;

/// The `pg_class` OID a sequence recorded when it was created.
fn sequence_oid(engine: &crate::Engine, relation: &uqa_core::RelationIdentity) -> i64 {
    let object_id = engine.durable.sequence_object_ids.read()[relation];
    uqa_execution::catalog::sequence::catalog_oids::sequence_catalog_oid(
        &engine.durable.sequence_catalog_oids.read(),
        &object_id,
    )
}
