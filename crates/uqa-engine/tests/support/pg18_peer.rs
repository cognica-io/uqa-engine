//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compare transactions on independent sessions of one database with a two-connection reference.

use crate::pg18_oracle::{error_matches, run_case};
use uqa_engine::Engine;

/// Replay a `PostgreSQL` reference through two connections to the same engine.
pub fn verify(engine: &Engine, transcript: &str) {
    let peer = engine.new_session().unwrap();
    let oracle: serde_json::Value = serde_json::from_str(transcript).unwrap();
    for schedule in oracle["schedules"].as_array().unwrap() {
        for step in schedule["steps"].as_array().unwrap() {
            let session = if step["session"] == "A" {
                engine
            } else {
                &peer
            };
            let sql = step["sql"].as_str().unwrap();
            let actual = run_case(session, sql);
            if let Some(state) = step["result"]["sqlstate"].as_str() {
                assert_eq!(
                    actual["error"]["sqlstate"], state,
                    "{}: {sql}: {actual}",
                    schedule["name"]
                );
                assert!(
                    error_matches(&actual["error"], &step["result"]),
                    "{sql}: {actual}"
                );
            } else {
                assert!(
                    actual["error"].is_null(),
                    "{}: {sql}: {actual}",
                    schedule["name"]
                );
                assert_eq!(
                    actual["results"][0]["rows"], step["result"]["rows"],
                    "{}: {sql}",
                    schedule["name"]
                );
            }
        }
    }
}
