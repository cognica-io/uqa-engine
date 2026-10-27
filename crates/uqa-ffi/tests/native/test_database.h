/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_TEST_DATABASE_H
#define UQA_TEST_DATABASE_H

#include <uqa/uqa.h>
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static inline const char *test_database_backend(void) {
    const char *backend = getenv("UQA_TEST_STORAGE");
    assert(backend != NULL);
    return backend;
}

static inline uqa_open_flags test_database_flags(void) {
    const char *backend = test_database_backend();
    if (strcmp(backend, "memory") == 0) return UQA_OPEN_MEMORY;
    if (strcmp(backend, "sqlite_native") == 0) return UQA_OPEN_SQLITE_NATIVE;
    if (strcmp(backend, "sqlite_kv") == 0) return UQA_OPEN_SQLITE_KV;
    if (strcmp(backend, "redb") == 0) return UQA_OPEN_REDB;
    fprintf(stderr, "unknown test storage: %s\n", backend);
    abort();
}

static inline uqa_status test_database_open_path(const char *path, uqa_config config, uqa_engine *out) {
    uqa_open_flags flags = test_database_flags();
    return uqa_engine_open(flags == UQA_OPEN_MEMORY ? NULL : path, flags, config, out);
}

static inline uqa_status test_database_open(uqa_config config, uqa_engine *out) {
    static unsigned int next_database = 0;
    const char *directory = getenv("UQA_TEST_DATABASE_DIR");
    char path[4096];
    int length;
    assert(directory != NULL);
    length = snprintf(path, sizeof(path), "%s/database-%u", directory, next_database++);
    assert(length > 0 && (size_t)length < sizeof(path));
    return test_database_open_path(path, config, out);
}

#endif
