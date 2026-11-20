//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

export function registerBackupTests(uqa) {
  for (const provider of ["memory", "sqlite"]) {
    test(`native ${provider} incremental backup restores after source and parent removal`, async () => {
      const root = mkdtempSync(join(tmpdir(), "uqa-node-backup-"));
      try {
        const sourcePath = join(root, "source");
        mkdirSync(sourcePath);
        const source = provider === "memory" ? new uqa.Engine() : uqa.openEncrypted(join(sourcePath, "database"), "node-source-key");
        assert.deepEqual(source.backupCapabilities(), { export: true, import: true, snapshotFingerprints: true });
        await source.sql("CREATE TABLE carried(id integer PRIMARY KEY, body text); INSERT INTO carried VALUES (1, repeat('a',65536)),(2,repeat('b',65536))");
        const options = {
          maxKeyBytes: 4096, maxFrameBytes: 16384, memoryBytes: 64 << 20,
          key: Buffer.alloc(32, 7), protectionDomain: Buffer.alloc(16, 9),
        };
        const location = join(root, "repository");
        if (provider === "sqlite") {
          const plain = await uqa.createBackupRepository(join(root, "plaintext"));
          await assert.rejects(source.beginBackup(plain));
          const permitted = await source.beginBackup(plain, { allowPlaintextExport: true });
          await permitted.cancel();
          await permitted.close();
          await plain.close();
        }
        await assert.rejects(async () => uqa.createBackupRepository(location, { maxFrameBytes: 1.5 }), /positive safe integer/);
        await assert.rejects(async () => uqa.createBackupRepository(location, { key: options.key }), /protectionDomain/);
        assert.equal(existsSync(location), false);
        let repository = await uqa.createBackupRepository(location, options);
        assert.ok(repository instanceof uqa.BackupRepository);
        assert.equal(repository.id.length, 16);
        await assert.rejects(async () => source.beginBackup(repository, { parent: Buffer.alloc(15) }), /16 bytes/);
        await assert.rejects(async () => source.beginBackup(repository, { memoryBytes: 1 }));
        const fullJob = await source.beginBackup(repository);
        assert.ok(fullJob instanceof uqa.BackupJob);
        assert.equal((await fullJob.progress()).phase, "capturing");
        assert.equal(await fullJob.report(), null);
        await repository.close();
        await repository.close();
        assert.throws(() => repository.list(), /repository is closed/);
        const full = await fullJob.finish();
        assert.deepEqual(full.id, fullJob.id);
        assert.equal(full.id.length, 16);
        assert.equal(full.parent, undefined);
        await fullJob.close();
        repository = await uqa.openBackupRepository(location, options);
        await source.sql("UPDATE carried SET body=repeat('c',65536) WHERE id=2; INSERT INTO carried VALUES (3,'child')");
        const childJob = await source.beginBackup(repository, { parent: full.id });
        assert.equal(await childJob.advance(1, 1024), "pending");
        while (await childJob.advance(64, 16384) === "pending") {}
        const report = await childJob.report();
        assert.ok(report.progress.reusedUnits > 0n);
        assert.ok(report.progress.writtenUnits > 0n);
        assert.ok(report.progress.certifiedReusedBytes >= 65536n);
        let child;
        if (provider === "sqlite") {
          const id = childJob.id;
          await childJob.close();
          child = await repository.resumePublication(id);
        } else {
          child = await childJob.publish();
          assert.deepEqual(await childJob.finish(), child);
          await childJob.close();
        }
        assert.deepEqual(child.parent, full.id);
        assert.deepEqual(await repository.resumePublication(child.id), child);
        const cancelled = await source.beginBackup(repository);
        assert.equal(await cancelled.cancel(), null);
        await assert.rejects(cancelled.publish());
        await cancelled.close();
        source.close();
        rmSync(sourcePath, { recursive: true });
        await assert.rejects(uqa.openBackupRepository(location, { ...options, key: Buffer.alloc(32, 8) }));
        await repository.close();
        repository = await uqa.openBackupRepository(location, options);
        await repository.remove(full.id);
        await assert.rejects(repository.verify(full.id));
        let removed = 0n;
        let collection;
        do {
          collection = await repository.collectGarbage(1);
          const files = collection.removedObjects + collection.removedStagingFiles;
          assert.ok(files <= 1n);
          removed += files;
        } while (collection.remainingFiles !== 0n);
        assert.ok(removed > 0n);
        assert.deepEqual(await repository.verify(child.id), child);
        const listing = await repository.list();
        assert.ok(listing instanceof uqa.BackupListing);
        await repository.close();
        assert.deepEqual(await listing.next(), child);
        assert.equal(await listing.next(), null);
        await listing.close();
        repository = await uqa.openBackupRepository(location, options);

        const replica = await uqa.createBackupRepository(join(root, "standalone"), {
          ...options, protectionDomain: Buffer.alloc(16, 10), key: Buffer.alloc(32, 11),
        });
        await assert.rejects(async () => repository.copyTo(child.id, replica, { parent: full.id }), /parent/);
        const detached = await repository.copyTo(child.id, replica);
        assert.equal(detached.parent, undefined);
        assert.deepEqual(detached.repository, replica.id);
        assert.deepEqual(detached.imageDigest, child.imageDigest);
        await repository.remove(child.id);
        await repository.close();
        rmSync(location, { recursive: true });
        repository = replica;
        assert.deepEqual(await repository.verify(detached.id), detached);
        const authority = new uqa.Engine();
        const destination = provider === "memory"
          ? { provider }
          : { provider, directory: join(root, "restored"), compression: {}, key: "node-restore-key" };
        const abandoned = await authority.beginRestore(repository, detached.id, destination);
        assert.equal(await abandoned.cancel(), null);
        assert.equal(await abandoned.phase(), "cancelled");
        await abandoned.close();
        if (destination.directory) assert.equal(existsSync(destination.directory), false);
        let job = await authority.beginRestore(repository, detached.id, destination);
        assert.ok(job instanceof uqa.RestoreJob);
        assert.deepEqual(job.backup, detached.id);
        assert.equal(await job.phase(), "reading");
        assert.equal(await job.advance(1), "reading");
        if (provider === "sqlite") {
          const id = job.id;
          assert.equal(id.length, 16);
          while (await job.advance(64 << 10) !== "prepared") {}
          await job.close();
          await repository.remove(detached.id);
          job = await authority.resumeRestore(repository, id, destination);
        } else {
          assert.equal(job.id, null);
        }
        let restored;
        if (provider === "sqlite") {
          while (restored == null) restored = await job.advancePublication(2048);
        } else {
          restored = await job.finish();
          await repository.remove(detached.id);
        }
        assert.ok(restored instanceof uqa.Engine);
        assert.equal(await job.phase(), "complete");
        await job.close();
        await repository.close();
        authority.close();
        const select = "SELECT id, substring(body,1,1) AS first FROM carried ORDER BY id";
        const expected = [{ id: 1, first: "a" }, { id: 2, first: "c" }, { id: 3, first: "c" }];
        assert.deepEqual((await restored.sql(select)).rows, expected);
        await restored.sql("INSERT INTO carried VALUES (4,'later')");
        expected.push({ id: 4, first: "l" });
        assert.deepEqual((await restored.sql(select)).rows, expected);
        restored.close();
        if (provider === "sqlite") {
          const reopened = uqa.openAuto(join(destination.directory, "database"), destination.key);
          assert.deepEqual((await reopened.sql(select)).rows, expected);
          reopened.close();
        }
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    });
  }
}
