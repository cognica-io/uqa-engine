import {
  BackupJob, BackupListing, BackupRepository, Engine, RestoreJob, createBackupRepository,
  type BackupProgress, type BackupRoot,
} from "@cognica-io/uqa";

export async function useBackupFactories(engine: Engine, path: string) {
  const repository: BackupRepository = await createBackupRepository(path);
  const job: BackupJob = await engine.beginBackup(repository);
  const progress: BackupProgress = await job.progress();
  const root: BackupRoot = await job.finish();
  const listing: BackupListing = await repository.list();
  const item: BackupRoot | null = await listing.next();
  const restore: RestoreJob = await engine.beginRestore(repository, root.id, { provider: "memory" });
  const restored: Engine = await restore.finish();
  return { progress, item, restored };
}

export function identifyBackupHandle(value: unknown) {
  if (value instanceof BackupJob) return value.report();
  if (value instanceof BackupListing) return value.next();
  if (value instanceof BackupRepository) return value.list();
  if (value instanceof RestoreJob) return value.phase();
}

export function rejectDirectConstruction() {
  // @ts-expect-error A capture job is created by an Engine.
  new BackupJob();
  // @ts-expect-error A listing is created by a repository.
  new BackupListing();
  // @ts-expect-error A repository is opened or created by a factory.
  new BackupRepository();
  // @ts-expect-error A restore job is created by an Engine.
  new RestoreJob();
}
