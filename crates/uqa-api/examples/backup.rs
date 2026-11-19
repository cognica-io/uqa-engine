//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture, verify and restore an independent incremental backup.
//! Run with `cargo run -p uqa-api --example backup`.

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use uqa_api::backup::{
        begin_backup, begin_restore, BackupCaptureStep, BackupLimits, BackupOptions,
        LocalRepository, Protection, RestoreDestination, RestoreOptions, RestorePhase,
        StorageReadControl,
    };
    use uqa_engine::Engine;

    let directory = tempfile::tempdir()?;
    let control = StorageReadControl::with_limit(32 << 20);
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        Protection::Plaintext,
        BackupLimits::new(4096, 16 << 10)?,
        &control,
    )?;
    let source = Engine::new();
    source.sql("CREATE TABLE notes(id integer PRIMARY KEY, body text)", &[])?;
    source.sql("INSERT INTO notes VALUES(1,'first')", &[])?;
    let capture = |parent| -> Result<_, Box<dyn std::error::Error>> {
        let mut options = BackupOptions::new(16 << 20, 16 << 20);
        options.parent = parent;
        let mut job = begin_backup(&source, &repository, options, &control)?;
        while job.advance(64, 16 << 10)? == BackupCaptureStep::Pending {}
        Ok(job.publish()?)
    };
    let full = capture(None)?;
    source.sql("INSERT INTO notes VALUES(2,'second')", &[])?;
    let child = capture(Some(full.backup()))?;
    drop(source);
    repository.remove_backup(full.backup(), &control)?;
    repository
        .open_backup(child.backup(), &control)?
        .verify(16 << 20, &control)?;

    let authority = Engine::new();
    let mut job = begin_restore(
        &authority,
        &repository,
        child.backup(),
        RestoreDestination::Memory,
        RestoreOptions::new(16 << 20, 8 << 20),
        &control,
    )?;
    while job.advance(16 << 10)? != RestorePhase::Prepared {}
    let restored = loop {
        if let Some(engine) = job.advance_publication(64)? {
            break engine;
        }
    };
    drop(job);
    for row in restored
        .sql("SELECT body FROM notes ORDER BY id", &[])?
        .rows
    {
        let Some(uqa_core::Value::Str(body)) = row.get("body") else {
            return Err("restored note has no text body".into());
        };
        println!("{body}");
    }
    restored.sql("INSERT INTO notes VALUES(3,'after restore')", &[])?;
    Ok(())
}

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
fn main() {
    eprintln!("This backup example requires a native filesystem target.");
}
