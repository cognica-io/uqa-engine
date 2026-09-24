//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{names, reopen, sessions, sql, Engine, Value};

fn verify(
    engine: &Engine,
    table: &str,
    domain: &str,
    table_names: &[String],
    domain_names: &[String],
    indexes: &[String],
) {
    assert_eq!(names(engine, table, false), table_names);
    assert_eq!(names(engine, domain, true), domain_names);
    let actual = sql(
        engine,
        &format!("SELECT indexname FROM pg_indexes WHERE tablename='{table}' ORDER BY indexname"),
    )
    .rows
    .into_iter()
    .map(|row| match &row["indexname"] {
        Value::Str(name) => name.clone(),
        other => panic!("index name: {other:?}"),
    })
    .collect::<Vec<_>>();
    assert_eq!(actual, indexes);
    assert!(table_names
        .iter()
        .chain(domain_names)
        .chain(indexes)
        .all(|name| name.len() <= 63));
}

#[test]
fn long_constraint_and_index_names_preserve_utf8_and_collision_suffixes_after_reopen() {
    for provider in 0..3 {
        for unicode in [false, true] {
            let (directory, engine, peer) = sessions(provider);
            drop(peer);
            let (t, c, d, length) = if unicode {
                ("한", "글", "디", 20)
            } else {
                ("t", "c", "d", 60)
            };
            let table = t.repeat(length);
            let column = c.repeat(length);
            let domain = d.repeat(length);
            let (plain, suffixed, domain_plain, domain_suffix, index_plain, index_suffix) =
                if unicode {
                    (
                        [(9, 8), (9, 9)],
                        [(8, 8), (9, 9)],
                        [19, 18],
                        [18, 17],
                        (9, 9),
                        (9, 9),
                    )
                } else {
                    (
                        [(27, 26), (28, 28)],
                        [(26, 26), (28, 27)],
                        [57, 54],
                        [56, 53],
                        (29, 29),
                        (29, 28),
                    )
                };
            let table_names = |parts: [(usize, usize); 2], suffix: &str| {
                let mut names = parts
                    .into_iter()
                    .zip(["not_null", "check"])
                    .map(|((a, b), label)| {
                        format!("{}_{}_{label}{suffix}", t.repeat(a), c.repeat(b))
                    })
                    .collect::<Vec<_>>();
                names.sort();
                names
            };
            let domain_names = |parts: [usize; 2], suffix: &str| {
                let mut names = parts
                    .into_iter()
                    .zip(["check", "not_null"])
                    .map(|(a, label)| format!("{}_{label}{suffix}", d.repeat(a)))
                    .collect::<Vec<_>>();
                names.sort();
                names
            };
            let blockers = table_names(plain, "")
                .into_iter()
                .chain(domain_names(domain_plain, ""))
                .map(|name| format!("CONSTRAINT \"{name}\" CHECK(x>0)"))
                .collect::<Vec<_>>()
                .join(",");
            sql(&engine, &format!("CREATE TABLE blockers(x int,{blockers}); CREATE DOMAIN \"{domain}\" AS int NOT NULL CHECK(VALUE>0); CREATE TABLE \"{table}\"(\"{column}\" int NOT NULL CHECK(\"{column}\">0)); CREATE INDEX ON \"{table}\"(\"{column}\"); CREATE INDEX ON \"{table}\"(\"{column}\")"));
            let mut indexes = vec![
                format!(
                    "{}_{}_idx",
                    t.repeat(index_plain.0),
                    c.repeat(index_plain.1)
                ),
                format!(
                    "{}_{}_idx1",
                    t.repeat(index_suffix.0),
                    c.repeat(index_suffix.1)
                ),
            ];
            indexes.sort();
            let tables = table_names(suffixed, "1");
            let domains = domain_names(domain_suffix, "1");
            verify(&engine, &table, &domain, &tables, &domains, &indexes);
            drop(engine);
            let engine = reopen(provider, &directory.path().join("table-locks.db"));
            verify(&engine, &table, &domain, &tables, &domains, &indexes);
        }
    }
}

#[test]
fn quoted_default_index_names_retain_case_punctuation_and_repeated_keys() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE TABLE \"Mixed Table\"(\"Value!\" int, note text); CREATE INDEX ON \"Mixed Table\"(\"Value!\"); CREATE INDEX ON \"Mixed Table\"(lower(note)); CREATE INDEX ON \"Mixed Table\"(\"Value!\", \"Value!\")");
        let rows = sql(
            &engine,
            "SELECT indexname FROM pg_indexes WHERE tablename='Mixed Table' ORDER BY indexname",
        )
        .rows;
        assert_eq!(
            rows.into_iter()
                .map(|row| row["indexname"].clone())
                .collect::<Vec<_>>(),
            [
                "Mixed Table_Value!_Value!1_idx",
                "Mixed Table_Value!_idx",
                "Mixed Table_lower_idx"
            ]
            .map(|name| Value::Str(name.into()))
        );
    }
}
