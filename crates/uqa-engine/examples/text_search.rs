//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
// Run with: cargo run -p uqa-engine --example text_search

use uqa_engine::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();

    engine.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
        &[],
    )?;
    engine.sql(
        "INSERT INTO notes (id, title, body) VALUES \
         (1, 'rust async', 'rust async story with futures and tokio runtime in rust'), \
         (2, 'rust generics', 'rust language guide a deep dive into rust generics and traits'), \
         (3, 'python web', 'python web frameworks flask and django and python tooling'), \
         (4, 'rust embedded', 'rust embedded systems on no_std targets and async drivers')",
        &[],
    )?;

    let result = engine.sql(
        "SELECT id, title, _score \
           FROM notes \
          WHERE text_match(body, 'rust async') \
          ORDER BY _score DESC \
          LIMIT 5",
        &[],
    )?;

    println!("Top 5 hits for 'rust async':");
    for row in &result.rows {
        let id = row.get("id").map(|v| format!("{v:?}")).unwrap_or_default();
        let title = row
            .get("title")
            .map(|v| format!("{v:?}"))
            .unwrap_or_default();
        let score = row
            .get("_score")
            .map(|v| format!("{v:?}"))
            .unwrap_or_default();
        println!("  id={id} title={title} score={score}");
    }
    Ok(())
}
