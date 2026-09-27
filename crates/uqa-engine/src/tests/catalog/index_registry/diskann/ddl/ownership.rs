//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn as_role(engine: &Engine, role: &str, label: &str, statement: &str) {
    sql(engine, &format!("SET ROLE {role}"));
    command(engine, OWNERS, label, statement);
    sql(engine, "RESET ROLE");
}

fn generation(engine: &Engine) -> uqa_storage::diskann_index::format::DiskANNGeneration {
    let table = engine
        .require_query_table("uqa_index_ownership_oracle.items")
        .unwrap();
    let metadata = table
        .vector_indexes
        .read()
        .get("multi_field")
        .unwrap()
        .diskann_query_metadata(&uqa_storage::read_control::StorageReadControl::with_limit(
            1 << 20,
        ))
        .unwrap()
        .unwrap();
    metadata.manifest.input().generation
}

#[test]
fn diskann_shared_index_authority_and_atomic_drop_match_postgresql() {
    super::super::definitions::owners(|engine| {
        sql(engine, "CREATE ROLE uqa_index_owner_schema; CREATE ROLE uqa_index_owner_table; CREATE ROLE uqa_index_owner_member INHERIT; CREATE ROLE uqa_index_owner_creator; CREATE ROLE uqa_index_owner_outsider; GRANT uqa_index_owner_table TO uqa_index_owner_member; CREATE SCHEMA uqa_index_ownership_oracle AUTHORIZATION uqa_index_owner_schema; GRANT USAGE,CREATE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_table,uqa_index_owner_creator; GRANT USAGE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_member,uqa_index_owner_outsider");
        sql(engine, "SET ROLE uqa_index_owner_table; CREATE TABLE uqa_index_ownership_oracle.items(id int,embedding vector(2),member_field vector(2),multi_field vector(2)); INSERT INTO uqa_index_ownership_oracle.items VALUES(1,ARRAY[1,0],ARRAY[0,1],ARRAY[-1,0]); CREATE INDEX owner_drop_idx ON uqa_index_ownership_oracle.items USING diskann(embedding); CREATE INDEX member_drop_idx ON uqa_index_ownership_oracle.items USING diskann(member_field); CREATE INDEX multi_owner_idx ON uqa_index_ownership_oracle.items USING diskann(multi_field); RESET ROLE; SET ROLE uqa_index_owner_creator; CREATE TABLE uqa_index_ownership_oracle.creator_items(embedding vector(2)); CREATE INDEX multi_creator_idx ON uqa_index_ownership_oracle.creator_items USING diskann(embedding); RESET ROLE");
        let original = generation(engine);
        for (role, label, statement) in [
            ("uqa_index_owner_creator", "creator-create", "CREATE INDEX denied ON uqa_index_ownership_oracle.items USING diskann(embedding)"),
            ("uqa_index_owner_schema", "schema-owner-create", "CREATE INDEX denied ON uqa_index_ownership_oracle.items USING diskann(embedding)"),
            ("uqa_index_owner_creator", "if-not-exists-still-checks-owner", "CREATE INDEX IF NOT EXISTS owner_drop_idx ON uqa_index_ownership_oracle.items USING diskann(embedding)"),
            ("uqa_index_owner_creator", "creator-drop", "DROP INDEX uqa_index_ownership_oracle.owner_drop_idx"),
            ("uqa_index_owner_member", "member-drop", "DROP INDEX uqa_index_ownership_oracle.member_drop_idx"),
            ("uqa_index_owner_member", "member-create", "CREATE INDEX member_created_idx ON uqa_index_ownership_oracle.items USING diskann(member_field)"),
        ] {
            as_role(engine, role, label, statement);
        }
        value(engine, OWNERS, "member-created-owner", "SELECT pg_get_userbyid(relowner) FROM pg_class WHERE oid='uqa_index_ownership_oracle.member_created_idx'::regclass");
        sql(
            engine,
            "REVOKE CREATE ON SCHEMA uqa_index_ownership_oracle FROM uqa_index_owner_table",
        );
        as_role(
            engine,
            "uqa_index_owner_table",
            "owner-without-create",
            "CREATE INDEX denied ON uqa_index_ownership_oracle.items USING diskann(absent)",
        );
        as_role(
            engine,
            "uqa_index_owner_creator",
            "nonowner-before-create-and-definition",
            "CREATE INDEX denied ON uqa_index_ownership_oracle.items USING diskann(absent)",
        );
        as_role(
            engine,
            "uqa_index_owner_table",
            "missing-table-before-create",
            "CREATE INDEX denied ON uqa_index_ownership_oracle.missing USING diskann(embedding)",
        );
        sql(engine, "GRANT CREATE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_table; REVOKE USAGE ON SCHEMA uqa_index_ownership_oracle FROM uqa_index_owner_table");
        as_role(
            engine,
            "uqa_index_owner_table",
            "owner-without-usage",
            "CREATE INDEX denied ON uqa_index_ownership_oracle.items USING diskann(absent)",
        );
        sql(
            engine,
            "GRANT USAGE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_table",
        );
        as_role(engine, "uqa_index_owner_table", "multi-target-atomic", "DROP INDEX uqa_index_ownership_oracle.multi_owner_idx,uqa_index_ownership_oracle.multi_creator_idx");
        value(engine, OWNERS, "after-multi-target", "SELECT string_agg(indexname,',' ORDER BY indexname) FROM pg_indexes WHERE schemaname='uqa_index_ownership_oracle' AND indexname IN ('multi_owner_idx','multi_creator_idx')");
        assert_eq!(generation(engine), original);
        assert_eq!(scalar(engine, "SELECT _score FROM uqa_index_ownership_oracle.items WHERE knn_match(multi_field,ARRAY[1,0],1)"), Value::Float(-1.0));
        as_role(
            engine,
            "uqa_index_owner_table",
            "if-exists-existing-checks-owner",
            "DROP INDEX IF EXISTS uqa_index_ownership_oracle.multi_creator_idx",
        );
        as_role(engine, "uqa_index_owner_schema", "schema-owner-multi-drop", "DROP INDEX uqa_index_ownership_oracle.multi_owner_idx,uqa_index_ownership_oracle.multi_creator_idx");
        assert_eq!(scalar(engine, "SELECT count(*) FROM pg_indexes WHERE schemaname='uqa_index_ownership_oracle' AND indexname IN ('multi_owner_idx','multi_creator_idx')"), Value::Int(0));
        as_role(
            engine,
            "uqa_index_owner_table",
            "owner-drop",
            "DROP INDEX uqa_index_ownership_oracle.owner_drop_idx",
        );
    });
}
