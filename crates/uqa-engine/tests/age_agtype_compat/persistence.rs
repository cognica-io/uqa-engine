//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

// ---------------------------------------------------------------------
// Label registry persistence
// ---------------------------------------------------------------------

#[test]
fn graphid_allocation_survives_engine_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("age_graphid.db");
    {
        let eng = Engine::open(&path).unwrap();
        exec(&eng, "SELECT create_graph('persist')");
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 CREATE (:Person {name: 'Alice'}), (:City {name: 'Seoul'})
             $$) AS (v agtype)",
        );
        // Delete the City so its label survives only via metadata.
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 MATCH (c:City) DETACH DELETE c
             $$) AS (v agtype)",
        );
    }
    {
        let eng = Engine::open(&path).unwrap();
        // Person keeps label id 3; the next Person is sequence 2.
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 CREATE (:Person {name: 'Bob'})
             $$) AS (v agtype)",
        );
        assert_eq!(
            agtype_rows(
                &eng,
                "persist",
                "MATCH (n:Person) RETURN id(n) ORDER BY id(n)"
            ),
            vec![
                Some("844424930131969".to_string()),
                Some("844424930131970".to_string()),
            ]
        );
        // City's label id (4) survives deletion of all its vertices,
        // so a NEW label continues at 5 and a recreated City vertex
        // resumes its sequence at 2.
        exec(
            &eng,
            "SELECT * FROM cypher('persist', $$
                 CREATE (:City {name: 'Busan'}), (:Country {name: 'KR'})
             $$) AS (v agtype)",
        );
        assert_agtype(
            &eng,
            "persist",
            "MATCH (c:City) RETURN id(c)",
            &((4_u64 << 48) | 2).to_string(),
        );
        assert_agtype(
            &eng,
            "persist",
            "MATCH (c:Country) RETURN id(c)",
            &((5_u64 << 48) | 1).to_string(),
        );
    }
}
