/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_UQA_BACKUP_H
#define UQA_UQA_BACKUP_H

#include <uqa/uqa.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct uqa_backup_repository_t *uqa_backup_repository;
typedef struct uqa_backup_list_t *uqa_backup_list;
typedef struct uqa_backup_job_t *uqa_backup_job;
typedef struct uqa_restore_job_t *uqa_restore_job;
typedef struct uqa_backup_id { uint8_t bytes[16]; } uqa_backup_id;
typedef struct uqa_restore_id { uint8_t bytes[16]; } uqa_restore_id;

/* NULL selects a plaintext repository. Encrypted repositories require the same
 * nonzero domain and 32-byte key on every open. The library copies both. */
typedef struct uqa_backup_encryption {
    uint8_t domain[16];
    uint8_t key[32];
} uqa_backup_encryption;

/* Pass NULL for native defaults. To override individual fields, initialize
 * the structure with its options_init call and sizeof. The capture option
 * allow_plaintext_export permits writing an encrypted source to a plaintext
 * repository when set to 1. Leave reserved fields zero. */
typedef struct uqa_backup_repository_options {
    uint32_t struct_size;
    uint32_t version;
    uint32_t maximum_key_bytes;
    uint32_t maximum_frame_bytes;
    uint64_t memory_limit_bytes;
    uint64_t maximum_workspace_bytes;
} uqa_backup_repository_options;

typedef struct uqa_backup_options {
    uint32_t struct_size;
    uint32_t version;
    uint64_t memory_limit_bytes;
    uint64_t maximum_encoded_bytes;
    uint64_t maximum_workspace_bytes;
    uint32_t allow_plaintext_export;
    uint32_t reserved;
} uqa_backup_options;

typedef struct uqa_restore_options {
    uint32_t struct_size;
    uint32_t version;
    uint64_t memory_limit_bytes;
    uint64_t maximum_workspace_bytes;
    uint64_t maximum_value_bytes;
    uint64_t retained_bytes;
} uqa_restore_options;

/* One bounded garbage collection pass. Byte counters use encoded file lengths. */
typedef struct uqa_backup_collection_report {
    uint64_t completed_backups;
    uint64_t prepared_backups;
    uint64_t active_operations;
    uint64_t referenced_frame_bytes;
    uint64_t retained_object_bytes;
    uint64_t unreferenced_object_bytes;
    uint64_t removed_objects;
    uint64_t removed_object_bytes;
    uint64_t removed_staging_files;
    uint64_t removed_staging_bytes;
    uint64_t remaining_files;
    uint64_t workspace_peak_bytes;
} uqa_backup_collection_report;

typedef uint32_t uqa_backup_phase;
#define UQA_BACKUP_CAPTURING UINT32_C(1)
#define UQA_BACKUP_VERIFYING UINT32_C(2)
#define UQA_BACKUP_PREPARED UINT32_C(3)
#define UQA_BACKUP_COMPLETE UINT32_C(4)
#define UQA_BACKUP_CANCELLING UINT32_C(5)
#define UQA_BACKUP_CANCELLED UINT32_C(6)
#define UQA_BACKUP_FAILED UINT32_C(7)

typedef uint32_t uqa_restore_phase;
#define UQA_RESTORE_READING UINT32_C(1)
#define UQA_RESTORE_VALIDATING UINT32_C(2)
#define UQA_RESTORE_PREPARED UINT32_C(3)
#define UQA_RESTORE_PUBLISHING UINT32_C(4)
#define UQA_RESTORE_COMPLETE UINT32_C(5)
#define UQA_RESTORE_CANCELLED UINT32_C(6)
#define UQA_RESTORE_FAILED UINT32_C(7)

UQA_API uqa_status UQA_CALL uqa_backup_repository_options_init(
    uqa_backup_repository_options *out, size_t size);
UQA_API uqa_status UQA_CALL uqa_backup_options_init(uqa_backup_options *out, size_t size);
UQA_API uqa_status UQA_CALL uqa_restore_options_init(uqa_restore_options *out, size_t size);

/* create requires a new directory. The parent must exist. Jobs and listings
 * own their data independently and may outlive the repository handle. */
UQA_API uqa_status UQA_CALL uqa_backup_repository_create(
    const char *path, const uqa_backup_encryption *encryption,
    const uqa_backup_repository_options *options, uqa_backup_repository *out);
UQA_API uqa_status UQA_CALL uqa_backup_repository_create_n(
    const char *path, size_t path_size, const uqa_backup_encryption *encryption,
    const uqa_backup_repository_options *options, uqa_backup_repository *out);
UQA_API uqa_status UQA_CALL uqa_backup_repository_open(
    const char *path, const uqa_backup_encryption *encryption,
    const uqa_backup_repository_options *options, uqa_backup_repository *out);
UQA_API uqa_status UQA_CALL uqa_backup_repository_open_n(
    const char *path, size_t path_size, const uqa_backup_encryption *encryption,
    const uqa_backup_repository_options *options, uqa_backup_repository *out);
UQA_API uqa_status UQA_CALL uqa_backup_repository_close(uqa_backup_repository *repository);
UQA_API uqa_status UQA_CALL uqa_backup_repository_verify(
    uqa_backup_repository repository, const uqa_backup_id *id);
/* Recover a durable prepared capture and publish its original snapshot. */
UQA_API uqa_status UQA_CALL uqa_backup_repository_resume_publication(
    uqa_backup_repository repository, const uqa_backup_id *id);
UQA_API uqa_status UQA_CALL uqa_backup_repository_remove(
    uqa_backup_repository repository, const uqa_backup_id *id);
/* Copy and verify a published image in an independent destination repository.
 * Returns its destination ID. Both repositories may use different keys. */
UQA_API uqa_status UQA_CALL uqa_backup_repository_copy(
    uqa_backup_repository source, const uqa_backup_id *id, uqa_backup_repository destination,
    const uqa_backup_options *options, uqa_backup_id *out);
/* Retired data can be collected in bounded passes until remaining_files is zero. */
UQA_API uqa_status UQA_CALL uqa_backup_repository_collect_garbage(
    uqa_backup_repository repository, size_t maximum_files, uqa_backup_collection_report *out);
UQA_API uqa_status UQA_CALL uqa_backup_list_create(
    uqa_backup_repository repository, uqa_backup_list *out);
/* Returns UQA_DONE after the last backup. A zero parent denotes a full backup. */
UQA_API uqa_status UQA_CALL uqa_backup_list_next(
    uqa_backup_list list, uqa_backup_id *id, uqa_backup_id *parent);
UQA_API uqa_status UQA_CALL uqa_backup_list_destroy(uqa_backup_list *list);

/* NULL parent selects a full backup. Creation captures the committed snapshot
 * and allows the source Engine to close while the job continues. */
UQA_API uqa_status UQA_CALL uqa_backup_job_create(
    uqa_engine engine, uqa_backup_repository repository, const uqa_backup_id *parent,
    const uqa_backup_options *options, uqa_backup_job *out);
UQA_API uqa_status UQA_CALL uqa_backup_job_get_id(uqa_backup_job job, uqa_backup_id *out);
UQA_API uqa_status UQA_CALL uqa_backup_job_get_phase(
    uqa_backup_job job, uqa_backup_phase *out);
UQA_API uqa_status UQA_CALL uqa_backup_job_advance(
    uqa_backup_job job, size_t maximum_positions, size_t maximum_bytes,
    uqa_backup_phase *out);
UQA_API uqa_status UQA_CALL uqa_backup_job_publish(uqa_backup_job job, uqa_backup_id *out);
/* Advance and publish the captured backup to completion. */
UQA_API uqa_status UQA_CALL uqa_backup_job_finish(uqa_backup_job job, uqa_backup_id *out);
UQA_API uqa_status UQA_CALL uqa_backup_job_cancel(uqa_backup_job job);
UQA_API uqa_status UQA_CALL uqa_backup_job_destroy(uqa_backup_job *job);

/* flags selects MEMORY, SQLITE_NATIVE, SQLITE_KV or REDB. Memory requires a
 * NULL path. Persistent restore installs a new database directory. config
 * configures the destination and may be NULL. The authorizing Engine may close
 * after create or resume. Independent host resources are retained by the job. */
UQA_API uqa_status UQA_CALL uqa_restore_job_create(
    uqa_engine engine, uqa_backup_repository repository, const uqa_backup_id *id,
    const char *path, uqa_open_flags flags, uqa_config config,
    const uqa_restore_options *options, uqa_restore_job *out);
UQA_API uqa_status UQA_CALL uqa_restore_job_create_n(
    uqa_engine engine, uqa_backup_repository repository, const uqa_backup_id *id,
    const char *path, size_t path_size, uqa_open_flags flags, uqa_config config,
    const uqa_restore_options *options, uqa_restore_job *out);
UQA_API uqa_status UQA_CALL uqa_restore_job_resume(
    uqa_engine engine, uqa_backup_repository repository, const uqa_restore_id *id,
    const char *path, uqa_open_flags flags, uqa_config config,
    const uqa_restore_options *options, uqa_restore_job *out);
UQA_API uqa_status UQA_CALL uqa_restore_job_resume_n(
    uqa_engine engine, uqa_backup_repository repository, const uqa_restore_id *id,
    const char *path, size_t path_size, uqa_open_flags flags, uqa_config config,
    const uqa_restore_options *options, uqa_restore_job *out);
/* Memory restore has no durable identifier and returns UQA_DONE. */
UQA_API uqa_status UQA_CALL uqa_restore_job_get_id(uqa_restore_job job, uqa_restore_id *out);
UQA_API uqa_status UQA_CALL uqa_restore_job_get_phase(
    uqa_restore_job job, uqa_restore_phase *out);
UQA_API uqa_status UQA_CALL uqa_restore_job_advance(
    uqa_restore_job job, size_t maximum_bytes, uqa_restore_phase *out);
/* After preparation, each successful call advances publication by at most the
 * supplied number of catalog steps. A pending call leaves *out NULL. The
 * completing call transfers a writable Engine to *out and clears *job. */
UQA_API uqa_status UQA_CALL uqa_restore_job_publish(
    uqa_restore_job *job, size_t maximum_steps, uqa_engine *out);
/* Advance and publish to completion, transferring the writable Engine to *out
 * and clearing *job. Destroy an unfinished job to release its resources. */
UQA_API uqa_status UQA_CALL uqa_restore_job_finish(uqa_restore_job *job, uqa_engine *out);
/* Cancellation before installation retires the private candidate. After
 * installation, finish transfers the previously published writable Engine. */
UQA_API uqa_status UQA_CALL uqa_restore_job_cancel(uqa_restore_job job);
UQA_API uqa_status UQA_CALL uqa_restore_job_destroy(uqa_restore_job *job);

#ifdef __cplusplus
}
#endif

#endif /* UQA_UQA_BACKUP_H */
