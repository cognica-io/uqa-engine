/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa_backup.h>
#include <uqa/uqa_udf.h>
#include "test_database.h"

static void check(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view message = {NULL, 0};
        if (uqa_error_get_last(&error) == UQA_OK)
            (void)uqa_error_get_message(error, &message);
        fprintf(stderr, "backup.c:%d: status %d: %.*s\n", line, (int)status,
                (int)message.size, message.data == NULL ? "" : message.data);
        abort();
    }
}
#define CHECK(call) check((call), __LINE__)

static void path_for(char *out, size_t size, const char *name) {
    int length = snprintf(out, size, "%s/%s", getenv("UQA_TEST_DATABASE_DIR"), name);
    assert(length > 0 && (size_t)length < size);
}

static void verify_rows(uqa_engine engine, uqa_result result, size_t count) {
    const int64_t expected[] = {7, 9, 12};
    uint64_t rows = 0;
    CHECK(uqa_engine_execute(engine, "SELECT id::bigint FROM inventory ORDER BY id", result));
    CHECK(uqa_result_get_row_count(result, &rows));
    assert(rows == count && count <= sizeof(expected) / sizeof(expected[0]));
    for (size_t i = 0; i < count; ++i) {
        uqa_value_view value = NULL;
        int64_t id = 0;
        CHECK(uqa_result_get_value(result, i, 0, &value));
        CHECK(uqa_value_get_int64(value, &id));
        assert(id == expected[i]);
    }
}

static void prepare_restore(uqa_restore_job job) {
    uqa_restore_phase phase = 0;
    CHECK(uqa_restore_job_get_phase(job, &phase));
    while (phase == UQA_RESTORE_READING || phase == UQA_RESTORE_VALIDATING)
        CHECK(uqa_restore_job_advance(job, 65536, &phase));
    assert(phase == UQA_RESTORE_PREPARED || phase == UQA_RESTORE_PUBLISHING ||
           phase == UQA_RESTORE_COMPLETE);
}

static uint64_t collect(uqa_backup_repository repository) {
    uqa_backup_collection_report report;
    uint64_t removed_objects = 0;
    do {
        CHECK(uqa_backup_repository_collect_garbage(repository, 32, &report));
        assert(report.removed_objects + report.removed_staging_files <= 32);
        removed_objects += report.removed_objects;
    } while (report.remaining_files != 0);
    return removed_objects;
}

static uqa_status UQA_CALL identity(uqa_udf_context context, const uqa_value_view *args,
                                    uint32_t count, void *user_data) {
    assert(count == 1);
    ++*(int *)user_data;
    return uqa_udf_context_set_result(context, args[0]);
}

int main(void) {
    const uqa_open_flags flags = test_database_flags();
    const int persistent = flags != UQA_OPEN_MEMORY;
    const uqa_backup_id zero_id = {{0}};
    uqa_backup_encryption encryption = {{1}, {2}}, wrong = encryption;
    uqa_backup_repository repository = NULL, rejected = NULL, copy = NULL;
    uqa_backup_repository_options repository_options;
    uqa_backup_options backup_options;
    uqa_restore_options restore_options;
    uqa_backup_list list = NULL;
    uqa_backup_job backup = NULL;
    uqa_restore_job restore = NULL;
    uqa_engine source = NULL, authority = NULL, restored = NULL;
    uqa_config target_config = NULL;
    uqa_result result = NULL;
    uqa_error error = NULL;
    uqa_udf function = NULL;
    int invocations = 0;
    uqa_backup_id full, incremental, id, parent;
    uqa_restore_id request;
    uqa_backup_phase phase = 0;
    uqa_restore_phase restore_phase = 0;
    char repository_path[4096], copy_path[4096], full_path[4096], incremental_path[4096], cancelled_path[4096];
    path_for(repository_path, sizeof(repository_path), "repository");
    path_for(copy_path, sizeof(copy_path), "copy");
    path_for(full_path, sizeof(full_path), "full");
    path_for(incremental_path, sizeof(incremental_path), "incremental");
    path_for(cancelled_path, sizeof(cancelled_path), "cancelled");
    const char *full_destination = persistent ? full_path : NULL;
    const char *incremental_destination = persistent ? incremental_path : NULL;

    CHECK(uqa_backup_repository_options_init(&repository_options, sizeof(repository_options)));
    CHECK(uqa_backup_options_init(&backup_options, sizeof(backup_options)));
    CHECK(uqa_restore_options_init(&restore_options, sizeof(restore_options)));
    assert(uqa_restore_options_init(&restore_options, sizeof(restore_options) - 1) ==
           UQA_ERROR_INVALID_ARGUMENT);
    CHECK(uqa_backup_repository_create_n(repository_path, strlen(repository_path), &encryption,
                                         &repository_options, &repository));
    wrong.key[0] ^= 1;
    assert(uqa_backup_repository_open(repository_path, &wrong, NULL, &rejected) ==
           UQA_ERROR_INTEGRITY);
    assert(rejected == NULL);
    CHECK(uqa_error_get_last(&error));
    assert(error != NULL);
    CHECK(uqa_backup_list_create(repository, &list));
    assert(uqa_backup_list_next(list, &id, &parent) == UQA_DONE);
    CHECK(uqa_backup_list_destroy(&list));

    CHECK(test_database_open(NULL, &source));
    CHECK(uqa_engine_open(NULL, UQA_OPEN_MEMORY, NULL, &authority));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(source, "CREATE TABLE inventory(id INT PRIMARY KEY); "
                             "INSERT INTO inventory VALUES (7)", result));
    backup_options.reserved = 1;
    assert(uqa_backup_job_create(source, repository, NULL, &backup_options, &backup) ==
           UQA_ERROR_INVALID_ARGUMENT);
    assert(backup == NULL);
    backup_options.reserved = 0;
    uint64_t encoded_limit = backup_options.maximum_encoded_bytes;
    backup_options.maximum_encoded_bytes = 1;
    assert(uqa_backup_job_create(source, repository, NULL, &backup_options, &backup) ==
           UQA_ERROR_LIMIT);
    assert(backup == NULL);
    backup_options.maximum_encoded_bytes = encoded_limit;
    CHECK(uqa_backup_job_create(source, repository, NULL, &backup_options, &backup));
    CHECK(uqa_backup_job_cancel(backup));
    CHECK(uqa_backup_job_get_phase(backup, &phase));
    assert(phase == UQA_BACKUP_CANCELLED);
    CHECK(uqa_backup_job_destroy(&backup));

    CHECK(uqa_backup_job_create(source, repository, NULL, NULL, &backup));
    CHECK(uqa_backup_job_get_id(backup, &full));
    CHECK(uqa_engine_execute(source, "INSERT INTO inventory VALUES (9)", result));
    CHECK(uqa_backup_job_get_phase(backup, &phase));
    while (phase == UQA_BACKUP_CAPTURING || phase == UQA_BACKUP_VERIFYING)
        CHECK(uqa_backup_job_advance(backup, 32, 65536, &phase));
    assert(phase == UQA_BACKUP_PREPARED);
    CHECK(uqa_backup_job_destroy(&backup));
    CHECK(uqa_backup_repository_resume_publication(repository, &full));
    CHECK(uqa_backup_repository_verify(repository, &full));

    CHECK(uqa_engine_execute(authority, "CREATE ROLE backup_reader; SET ROLE backup_reader", result));
    assert(uqa_restore_job_create(authority, repository, &full, full_destination, flags, NULL,
                                  NULL, &restore) == UQA_ERROR_SQL);
    assert(restore == NULL);
    CHECK(uqa_error_get_last(&error));
    uqa_string_view state = {NULL, 0};
    CHECK(uqa_error_get_sqlstate(error, &state));
    assert(state.size == 5 && memcmp(state.data, "42501", 5) == 0);
    CHECK(uqa_engine_execute(authority, "RESET ROLE", result));
    CHECK(uqa_restore_job_create(authority, repository, &full, persistent ? cancelled_path : NULL,
                                 flags, NULL, NULL, &restore));
    CHECK(uqa_restore_job_cancel(restore));
    CHECK(uqa_restore_job_get_phase(restore, &restore_phase));
    assert(restore_phase == UQA_RESTORE_CANCELLED);
    CHECK(uqa_restore_job_destroy(&restore));
    CHECK(uqa_restore_job_create(authority, repository, &full, full_destination, flags, NULL,
                                 NULL, &restore));
    CHECK(uqa_restore_job_finish(&restore, &restored));
    assert(restore == NULL && restored != NULL);
    verify_rows(restored, result, 1);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&restored));

    CHECK(uqa_backup_job_create(source, repository, &full, &backup_options, &backup));
    CHECK(uqa_engine_close(&source));
    CHECK(uqa_backup_repository_close(&repository));
    CHECK(uqa_backup_job_finish(backup, &incremental));
    assert(memcmp(&incremental, &full, sizeof(full)) != 0);
    CHECK(uqa_backup_job_publish(backup, &id));
    assert(memcmp(&id, &incremental, sizeof(id)) == 0);
    CHECK(uqa_backup_job_destroy(&backup));
    CHECK(uqa_backup_repository_open_n(repository_path, strlen(repository_path), &encryption,
                                      NULL, &repository));
    CHECK(uqa_backup_repository_remove(repository, &full));
    (void)collect(repository);
    CHECK(uqa_backup_repository_verify(repository, &incremental));
    CHECK(uqa_backup_list_create(repository, &list));
    CHECK(uqa_backup_repository_close(&repository));
    CHECK(uqa_backup_list_next(list, &id, &parent));
    assert(memcmp(&id, &incremental, sizeof(id)) == 0);
    assert(memcmp(&parent, &full, sizeof(parent)) == 0);
    assert(uqa_backup_list_next(list, &id, &parent) == UQA_DONE);
    assert(memcmp(&id, &zero_id, sizeof(id)) == 0);
    CHECK(uqa_backup_list_destroy(&list));
    CHECK(uqa_backup_repository_open(repository_path, &encryption, NULL, &repository));

    {
        char plain_path[4096];
        uqa_backup_repository plaintext = NULL;
        path_for(plain_path, sizeof(plain_path), "plaintext");
        CHECK(uqa_backup_repository_create(plain_path, NULL, NULL, &plaintext));
        assert(uqa_backup_repository_copy(repository, &incremental, plaintext, NULL, &id) ==
               UQA_ERROR_INVALID_ARGUMENT);
        assert(memcmp(&id, &zero_id, sizeof(id)) == 0);
        CHECK(uqa_backup_repository_close(&plaintext));
    }
    uqa_backup_encryption copy_key = encryption;
    copy_key.domain[1] = 8;
    copy_key.key[1] = 9;
    CHECK(uqa_backup_repository_create(copy_path, &copy_key, NULL, &copy));
    id = incremental;
    CHECK(uqa_backup_repository_copy(repository, &id, copy, &backup_options, &id));
    CHECK(uqa_backup_repository_verify(copy, &id));
    CHECK(uqa_backup_repository_remove(repository, &incremental));
    assert(collect(repository) > 0);
    CHECK(uqa_backup_repository_close(&repository));
    CHECK(uqa_backup_repository_close(&copy));
    CHECK(uqa_backup_repository_open(copy_path, &copy_key, NULL, &repository));
    incremental = id;
    CHECK(uqa_backup_repository_verify(repository, &incremental));

    CHECK(uqa_udf_register_scalar(authority, "backup_identity", NULL, identity, &invocations,
                                  NULL, &function));
    if (flags == UQA_OPEN_SQLITE_NATIVE || flags == UQA_OPEN_SQLITE_KV) {
        CHECK(uqa_config_create(&target_config));
        CHECK(uqa_config_set_encryption_key(target_config, "restored-database-test-key"));
        CHECK(uqa_config_set_compression(target_config, UQA_COMPRESSION_ZSTD, 3));
    }
    CHECK(uqa_restore_job_create_n(authority, repository, &incremental, incremental_destination,
                                  persistent ? strlen(incremental_destination) : 0,
                                  flags, target_config, &restore_options, &restore));
    prepare_restore(restore);
    if (persistent) {
        CHECK(uqa_restore_job_get_id(restore, &request));
        CHECK(uqa_restore_job_destroy(&restore));
        CHECK(uqa_restore_job_resume_n(authority, repository, &request, incremental_path,
                                      strlen(incremental_path), flags, target_config, NULL, &restore));
        prepare_restore(restore);
    } else {
        assert(uqa_restore_job_get_id(restore, &request) == UQA_DONE);
    }
    CHECK(uqa_engine_close(&authority));
    CHECK(uqa_backup_repository_close(&repository));
    while (restored == NULL)
        CHECK(uqa_restore_job_publish(&restore, 4, &restored));
    assert(restore == NULL);
    CHECK(uqa_result_create(&result));
    verify_rows(restored, result, 2);
    CHECK(uqa_engine_execute(restored, "SELECT backup_identity(id) FROM inventory", result));
    assert(invocations == 2);
    CHECK(uqa_udf_unregister(&function));
    CHECK(uqa_engine_execute(restored, "INSERT INTO inventory VALUES (12)", result));
    verify_rows(restored, result, 3);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&restored));
    if (persistent) {
        char installed[4096];
        path_for(installed, sizeof(installed), "incremental/database");
        CHECK(uqa_engine_open(installed, flags, target_config, &restored));
        CHECK(uqa_result_create(&result));
        verify_rows(restored, result, 3);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&restored));
    }
    CHECK(uqa_config_destroy(&target_config));
    CHECK(uqa_restore_job_destroy(&restore));
    return 0;
}
