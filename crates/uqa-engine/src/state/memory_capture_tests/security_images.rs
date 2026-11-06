//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::catalog::{
    roles::{RoleDefinitions, RoleMemberships},
    security::{database::DatabaseAuthority, schema_registry::SchemaRegistry},
};

#[test]
fn captured_security_images_restore_original_authority_after_replacement_and_engine_closure() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE team; CREATE ROLE member LOGIN CONNECTION LIMIT 7; GRANT team TO member WITH ADMIN OPTION; CREATE SCHEMA saved AUTHORIZATION team; GRANT USAGE ON SCHEMA saved TO member WITH GRANT OPTION; GRANT CREATE ON DATABASE uqa TO member");
    let roles = engine.durable.roles.snapshot();
    let memberships = engine.durable.role_memberships.snapshot();
    let schemas = engine.durable.schemas.snapshot();
    let database = engine.durable.database_security.snapshot();
    let role_key = engine.durable.roles.key().unwrap();
    let membership_key = engine.durable.role_memberships.key().unwrap();
    let schema_key = engine.durable.schemas.key().unwrap();
    let database_key = engine.durable.database_security.key().unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    sql(&engine, "BEGIN; ALTER ROLE member CONNECTION LIMIT 3; REVOKE team FROM member; SAVEPOINT changed; DROP SCHEMA saved; ROLLBACK TO changed; ROLLBACK");
    sql(&engine, "REVOKE CREATE ON DATABASE uqa FROM member; DROP SCHEMA saved; REVOKE team FROM member; DROP ROLE member; CREATE ROLE member NOLOGIN; ALTER ROLE team RENAME TO replaced");
    assert_ne!(
        engine.durable.roles.read()["member"].object_id,
        roles["member"].object_id
    );
    drop(engine);
    let saved_roles = role_key.retained(&cut).unwrap().unwrap();
    let saved_memberships = membership_key.retained(&cut).unwrap().unwrap();
    let saved_schemas = schema_key.retained(&cut).unwrap().unwrap();
    let saved_database = database_key.retained(&cut).unwrap().unwrap();
    drop(cut);
    let token = CancellationToken::new();
    let mut role_image = vec![];
    saved_roles
        .get()
        .unwrap()
        .write_image(&mut role_image, &token)
        .unwrap();
    let mut membership_image = vec![];
    saved_memberships
        .get()
        .unwrap()
        .write_image(&mut membership_image, &token)
        .unwrap();
    let mut schema_image = vec![];
    saved_schemas
        .get()
        .unwrap()
        .write_image(&mut schema_image, &token)
        .unwrap();
    let mut database_image = vec![];
    saved_database
        .get()
        .unwrap()
        .write_image(&mut database_image, &token)
        .unwrap();
    drop((
        saved_roles,
        saved_memberships,
        saved_schemas,
        saved_database,
    ));
    assert_eq!(control.memory().used(), 0);
    let memory = MemoryBudget::new(1 << 20);
    let restored_roles =
        RoleDefinitions::read_image(&mut role_image.as_slice(), &memory, &token).unwrap();
    let restored_memberships =
        RoleMemberships::read_image(&mut membership_image.as_slice(), &memory, &token).unwrap();
    let restored_schemas =
        SchemaRegistry::read_image(&mut schema_image.as_slice(), &memory, &token).unwrap();
    let restored_database =
        DatabaseAuthority::read_image(&mut database_image.as_slice(), &memory, &token).unwrap();
    assert!(restored_roles == *roles);
    assert!(restored_memberships == *memberships);
    assert!(restored_schemas == *schemas);
    assert_eq!(restored_database, *database);
    restored_database.validate(&restored_roles).unwrap();
    for (_, schema) in &restored_schemas {
        schema.validate(&restored_roles).unwrap();
    }
    assert!(uqa_sql::catalog::roles::role_inherits(
        &restored_roles,
        &restored_memberships,
        "member",
        "team"
    ));
    drop((
        restored_roles,
        restored_memberships,
        restored_schemas,
        restored_database,
    ));
    assert_eq!(memory.used(), 0);
}
