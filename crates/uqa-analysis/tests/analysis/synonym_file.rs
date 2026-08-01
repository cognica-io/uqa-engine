//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::fs;
use tempfile::TempDir;

fn write_synonyms(dir: &TempDir, body: &str) -> std::path::PathBuf {
    let path = dir.path().join("synonyms.txt");
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn explicit_mapping() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(
        &dir,
        "# vehicle synonyms\ncar => automobile, vehicle\nfast => quick, speedy\n",
    );
    let f = TokenFilter::synonym_from_path(&path).unwrap();
    assert_eq!(
        f.filter(vec!["car".into()]).unwrap(),
        vec!["car", "automobile", "vehicle"]
    );
    assert_eq!(
        f.filter(vec!["fast".into()]).unwrap(),
        vec!["fast", "quick", "speedy"]
    );
    assert_eq!(f.filter(vec!["slow".into()]).unwrap(), vec!["slow"]);
}

#[test]
fn equivalent_synonyms() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(&dir, "car, automobile, vehicle\n");
    let f = TokenFilter::synonym_from_path(&path).unwrap();
    assert_eq!(
        f.filter(vec!["car".into()]).unwrap(),
        vec!["car", "automobile", "vehicle"]
    );
    assert_eq!(
        f.filter(vec!["automobile".into()]).unwrap(),
        vec!["automobile", "car", "vehicle"]
    );
    assert_eq!(
        f.filter(vec!["vehicle".into()]).unwrap(),
        vec!["vehicle", "car", "automobile"]
    );
}

#[test]
fn mixed_formats() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(
        &dir,
        "# explicit\nbig => large\n\n# equivalent\nfast, quick, speedy\n",
    );
    let f = TokenFilter::synonym_from_path(&path).unwrap();
    assert_eq!(f.filter(vec!["big".into()]).unwrap(), vec!["big", "large"]);
    // explicit one-way: large does not expand
    assert_eq!(f.filter(vec!["large".into()]).unwrap(), vec!["large"]);
    assert_eq!(
        f.filter(vec!["fast".into()]).unwrap(),
        vec!["fast", "quick", "speedy"]
    );
    assert_eq!(
        f.filter(vec!["quick".into()]).unwrap(),
        vec!["quick", "fast", "speedy"]
    );
}

#[test]
fn blank_lines_and_comments() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(
        &dir,
        "# this is a comment\n\n   \n# another comment\na => b\n",
    );
    let f = TokenFilter::synonym_from_path(&path).unwrap();
    assert_eq!(f.filter(vec!["a".into()]).unwrap(), vec!["a", "b"]);
}

#[test]
fn deduplication() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(&dir, "car => automobile\ncar => automobile, vehicle\n");
    let f = TokenFilter::synonym_from_path(&path).unwrap();
    assert_eq!(
        f.filter(vec!["car".into()]).unwrap(),
        vec!["car", "automobile", "vehicle"]
    );
}

#[test]
fn serialization_roundtrip() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(&dir, "car => automobile\n");
    let f = TokenFilter::synonym_from_path(&path).unwrap();
    let json = serde_json::to_string(&f).unwrap();
    assert!(json.contains("synonyms_path"));
    // Inline synonyms map is empty when sourced from file, so it's
    // omitted by the skip-if-empty serde attribute.
    assert!(!json.contains("\"synonyms\":{"));
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.filter(vec!["car".into()]).unwrap(),
        vec!["car", "automobile"]
    );
}

#[test]
fn inline_does_not_serialize_path() {
    let mut m = BTreeMap::new();
    m.insert("a".to_string(), vec!["b".to_string()]);
    let f = TokenFilter::Synonym {
        synonyms: m,
        synonyms_path: None,
    };
    let json = serde_json::to_string(&f).unwrap();
    assert!(json.contains("\"synonyms\""));
    assert!(!json.contains("synonyms_path"));
}

#[test]
fn file_not_found() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nonexistent.txt");
    let r = TokenFilter::synonym_from_path(&path);
    assert!(r.is_err());
}

#[test]
fn single_term_line_ignored() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(&dir, "lonely\ncar, automobile\n");
    let f = TokenFilter::synonym_from_path(&path).unwrap();
    assert_eq!(f.filter(vec!["lonely".into()]).unwrap(), vec!["lonely"]);
    assert_eq!(
        f.filter(vec!["car".into()]).unwrap(),
        vec!["car", "automobile"]
    );
}

#[test]
fn parse_helper_returns_map() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(&dir, "car => automobile\n");
    let map = TokenFilter::parse_synonym_file(&path).unwrap();
    assert_eq!(map.get("car"), Some(&vec!["automobile".to_string()]));
}

#[test]
fn file_path_filter_in_pipeline() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(&dir, "fast, quick\n");
    let synonym = TokenFilter::synonym_from_path(&path).unwrap();
    let pipe = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Lowercase, synonym],
        Vec::new(),
    );
    let result = pipe.analyze("Fast running").unwrap();
    assert!(result.contains(&"fast".to_string()));
    assert!(result.contains(&"quick".to_string()));
}

#[test]
fn deleting_synonym_file_is_an_execution_error() {
    let dir = TempDir::new().unwrap();
    let path = write_synonyms(&dir, "fast, quick\n");
    let filter = TokenFilter::synonym_from_path(&path).unwrap();
    fs::remove_file(&path).unwrap();

    let error = filter.filter(vec!["fast".into()]).unwrap_err();
    assert!(matches!(
        error,
        AnalysisError::SynonymFile(SynonymFileError::NotFound(missing))
            if missing == path
    ));
}
