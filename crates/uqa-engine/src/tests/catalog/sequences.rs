//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

mod alteration_authority;
mod dependencies;
mod lifecycle;
mod native;
mod ownership;
mod positions;
mod privileges;
mod removal;
mod restoration;
mod values;

/// The `pg_class` OID a sequence recorded when it was created.
fn sequence_oid(engine: &crate::Engine, relation: &uqa_core::RelationIdentity) -> i64 {
    let object_id = *engine.durable.sequences.read().object_id(relation).unwrap();
    uqa_execution::catalog::sequence::catalog_oids::sequence_catalog_oid(
        engine.durable.sequences.read().catalog_oids(),
        &object_id,
    )
}
