/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */
#include <uqa/uqa.h>
#include <libxml/parser.h>
#include <libxml/xmlmemory.h>
#include <assert.h>
#include <stdlib.h>
#include <string.h>

static size_t allocations;
static size_t releases;
static void *host_malloc(size_t size) { ++allocations; return malloc(size); }
static void *host_realloc(void *pointer, size_t size) { ++allocations; return realloc(pointer, size); }
static void host_free(void *pointer) { if (pointer != NULL) ++releases; free(pointer); }
static char *host_strdup(const char *text) {
    size_t size = strlen(text) + 1;
    char *copy = (char *)host_malloc(size);
    if (copy != NULL) memcpy(copy, text, size);
    return copy;
}

int main(void) {
    assert(xmlMemSetup(host_free, host_malloc, host_realloc, host_strdup) == 0);
    xmlInitParser();
    xmlDocPtr host = xmlReadMemory("<host/>", 7, NULL, NULL, 0);
    assert(host != NULL);
    assert(allocations != 0);
    size_t before_allocations = allocations;
    size_t before_releases = releases;
    for (unsigned index = 0; index < 32; ++index) {
        uqa_value value = NULL;
        assert(uqa_value_create_xml("<a/><b/>", &value) == UQA_OK);
        assert(uqa_value_destroy(&value) == UQA_OK);
        assert(uqa_value_create_xml("<a>", &value) == UQA_ERROR_SQL);
        assert(value == NULL);
    }
    assert(allocations == before_allocations && releases == before_releases);
    xmlFreeFunc free_callback;
    xmlMallocFunc malloc_callback;
    xmlReallocFunc realloc_callback;
    xmlStrdupFunc strdup_callback;
    assert(xmlMemGet(&free_callback, &malloc_callback, &realloc_callback, &strdup_callback) == 0);
    assert(free_callback == host_free && malloc_callback == host_malloc);
    assert(realloc_callback == host_realloc && strdup_callback == host_strdup);
    assert(strcmp((const char *)xmlDocGetRootElement(host)->name, "host") == 0);
    xmlFreeDoc(host);
    xmlCleanupParser();
    return 0;
}
