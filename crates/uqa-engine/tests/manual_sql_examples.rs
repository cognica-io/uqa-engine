//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compile every manual SQL block and execute blocks marked `sql execute`.

use std::fs;
use std::path::{Path, PathBuf};

use uqa_engine::Engine;

#[derive(Clone, Copy)]
enum ExampleMode {
    Compile,
    Execute,
    CompileFail,
}

struct SqlExample {
    line: usize,
    mode: ExampleMode,
    sql: String,
}

struct OpenFence {
    marker: char,
    length: usize,
    sql: Option<(usize, ExampleMode, String)>,
}

#[test]
fn manual_sql_examples_compile_or_execute() {
    let manual_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/manual");
    let mut markdown_files = Vec::new();
    collect_markdown_files(&manual_root, &mut markdown_files);
    markdown_files.sort();

    let mut example_count = 0;
    let mut executed_count = 0;

    for path in markdown_files {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let examples = parse_sql_examples(&path, &source);
        let engine = Engine::new();

        for example in examples {
            example_count += 1;
            let location = format!("{}:{}", path.display(), example.line);
            match example.mode {
                ExampleMode::Compile => {
                    let statements = uqa_sql::compile(&example.sql).unwrap_or_else(|error| {
                        panic!(
                            "manual SQL must compile at {location}: {error}\n{}",
                            example.sql
                        )
                    });
                    assert!(
                        !statements.is_empty(),
                        "manual SQL compiled to no statements at {location}:\n{}",
                        example.sql
                    );
                }
                ExampleMode::Execute => {
                    let statements = uqa_sql::compile(&example.sql).unwrap_or_else(|error| {
                        panic!(
                            "manual SQL must compile at {location}: {error}\n{}",
                            example.sql
                        )
                    });
                    assert!(
                        !statements.is_empty(),
                        "manual SQL compiled to no statements at {location}:\n{}",
                        example.sql
                    );
                    engine.sql(&example.sql, &[]).unwrap_or_else(|error| {
                        panic!(
                            "manual SQL must execute at {location}: {error}\n{}",
                            example.sql
                        )
                    });
                    executed_count += 1;
                }
                ExampleMode::CompileFail => {
                    assert!(
                        uqa_sql::compile(&example.sql).is_err(),
                        "manual SQL marked compile-fail compiled at {location}:\n{}",
                        example.sql
                    );
                }
            }
        }
    }

    assert!(example_count > 0, "manual contains no SQL examples");
    assert!(
        executed_count > 0,
        "manual must contain at least one `sql execute` example"
    );
}

fn collect_markdown_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read directory {}: {error}", directory.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            panic!("read directory entry in {}: {error}", directory.display())
        });
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
}

fn parse_sql_examples(path: &Path, source: &str) -> Vec<SqlExample> {
    let mut examples = Vec::new();
    let mut open: Option<OpenFence> = None;

    for (index, line) in source.lines().enumerate() {
        let line_number = index + 1;
        if let Some(fence) = open.as_mut() {
            if is_closing_fence(line, fence.marker, fence.length) {
                let closed = open.take().expect("open fence exists");
                if let Some((opening_line, mode, sql)) = closed.sql {
                    assert!(
                        !sql.trim().is_empty(),
                        "empty manual SQL block at {}:{opening_line}",
                        path.display()
                    );
                    examples.push(SqlExample {
                        line: opening_line,
                        mode,
                        sql,
                    });
                }
            } else if let Some((_, _, sql)) = fence.sql.as_mut() {
                sql.push_str(line);
                sql.push('\n');
            }
            continue;
        }

        let Some((marker, length, info)) = opening_fence(line) else {
            continue;
        };
        let mut words = info.split_whitespace();
        let language = words.next();
        let sql = if language == Some("sql") {
            let mode = match words.next() {
                None => ExampleMode::Compile,
                Some("execute") => ExampleMode::Execute,
                Some("compile-fail") => ExampleMode::CompileFail,
                Some(option) => panic!(
                    "unknown manual SQL fence option `{option}` at {}:{line_number}",
                    path.display()
                ),
            };
            assert!(
                words.next().is_none(),
                "manual SQL fence accepts one option at {}:{line_number}",
                path.display()
            );
            Some((line_number, mode, String::new()))
        } else {
            None
        };
        open = Some(OpenFence {
            marker,
            length,
            sql,
        });
    }

    assert!(
        open.is_none(),
        "unclosed Markdown fence in {}",
        path.display()
    );
    examples
}

fn opening_fence(line: &str) -> Option<(char, usize, &str)> {
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let content = &line[indentation..];
    let marker = content.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let length = content
        .chars()
        .take_while(|character| *character == marker)
        .count();
    if length < 3 {
        return None;
    }
    let info = &content[length..];
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some((marker, length, info.trim()))
}

fn is_closing_fence(line: &str, marker: char, opening_length: usize) -> bool {
    let content = line.trim_start_matches(' ');
    if line.len() - content.len() > 3 {
        return false;
    }
    let length = content
        .chars()
        .take_while(|character| *character == marker)
        .count();
    length >= opening_length && content[length..].trim().is_empty()
}
