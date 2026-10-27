/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_EXAMPLE_H
#define UQA_EXAMPLE_H

#include <uqa/uqa.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Keep checks active in release builds. A failed example exits immediately. */
static inline void example_check(uqa_status status, const char *call) {
    if (status == UQA_OK) return;
    uqa_error error = NULL;
    uqa_string_view message = {NULL, 0};
    (void)uqa_error_get_last(&error);
    if (error != NULL) (void)uqa_error_get_message(error, &message);
    fprintf(stderr, "%s failed (%d): ", call, (int)status);
    if (message.data != NULL) fwrite(message.data, 1, message.size, stderr);
    fputc('\n', stderr);
    exit(EXIT_FAILURE);
}

#define CHECK(call) example_check((call), #call)
#define REQUIRE(condition) do { \
    if (!(condition)) { \
        fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #condition); \
        exit(EXIT_FAILURE); \
    } \
} while (0)

typedef struct example_database {
    const char *path;
    uint32_t flags;
} example_database;

static inline example_database example_arguments(int argc, char **argv, int persistent) {
    example_database database = {NULL, UQA_OPEN_MEMORY};
    if (argc == 3) {
        database.path = argv[2];
        if (strcmp(argv[1], "sqlite-native") == 0) database.flags = UQA_OPEN_SQLITE_NATIVE;
        else if (strcmp(argv[1], "sqlite-kv") == 0) database.flags = UQA_OPEN_SQLITE_KV;
        else if (strcmp(argv[1], "redb") == 0) database.flags = UQA_OPEN_REDB;
        else database.flags = 0;
    } else if (argc != 1) database.flags = 0;
    if (database.flags == 0 || (persistent && database.path == NULL)) {
        fprintf(stderr, "Usage: %s%s sqlite-native|sqlite-kv|redb NEW_DATABASE_PATH%s\n",
                argv[0], persistent ? "" : " [", persistent ? "" : "]");
        exit(EXIT_FAILURE);
    }
    return database;
}

static inline uqa_engine example_open(example_database database) {
    uqa_engine engine = NULL;
    CHECK(uqa_engine_open(database.path, database.flags, NULL, &engine));
    return engine;
}

static inline uqa_result example_query(uqa_engine engine, const char *sql) {
    uqa_result result = NULL;
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine, sql, result));
    return result;
}

static inline void example_execute(uqa_engine engine, const char *sql) {
    uqa_result result = example_query(engine, sql);
    CHECK(uqa_result_destroy(&result));
}

static inline uint64_t example_rows(uqa_result result) {
    uint64_t count = 0;
    CHECK(uqa_result_get_row_count(result, &count));
    return count;
}

static inline uqa_value_view example_cell(uqa_result result, uint64_t row, uint32_t column) {
    uqa_value_view value = NULL;
    CHECK(uqa_result_get_value(result, row, column, &value));
    return value;
}

static inline int64_t example_integer(uqa_result result, uint64_t row, uint32_t column) {
    int64_t value = 0;
    CHECK(uqa_value_get_int64(example_cell(result, row, column), &value));
    return value;
}

static inline double example_float(uqa_result result, uint64_t row, uint32_t column) {
    double value = 0;
    CHECK(uqa_value_get_float64(example_cell(result, row, column), &value));
    return value;
}

static inline void example_text(uqa_result result, uint64_t row, uint32_t column,
                                const char *expected) {
    uqa_string_view value = {NULL, 0};
    CHECK(uqa_value_get_text(example_cell(result, row, column), &value));
    REQUIRE(value.size == strlen(expected));
    REQUIRE(memcmp(value.data, expected, value.size) == 0);
}

static inline void example_ids(uqa_result result, const int64_t *ids, size_t count) {
    REQUIRE(example_rows(result) == count);
    for (size_t i = 0; i < count; ++i) REQUIRE(example_integer(result, i, 0) == ids[i]);
}

static inline int64_t example_one_integer(uqa_engine engine, const char *sql) {
    uqa_result result = example_query(engine, sql);
    REQUIRE(example_rows(result) == 1);
    int64_t value = example_integer(result, 0, 0);
    CHECK(uqa_result_destroy(&result));
    return value;
}

#endif /* UQA_EXAMPLE_H */
