/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "native.h"
#include "allocation.h"
#include <libxml/parser.h>
#include <libxml/chvalid.h>
#include <libxml/tree.h>
#include <libxml/xmlerror.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

struct uqa_sql_xml_notice {
    struct uqa_sql_xml_notice *next;
    char *message;
    int level;
};

struct xml_error {
    char *detail;
    size_t length;
    int out_of_memory;
    xmlParserCtxt *parser;
    uqa_sql_xml_notice *notices;
    uqa_sql_xml_notice *last_notice;
};

struct xml_source {
    const char *input;
    size_t length;
    size_t offset;
};

static int read_input(void *context, char *buffer, int size) {
    struct xml_source *source = context;
    if (uqa_sql_xml_check() != 0) return -1;
    size_t length = source->length - source->offset;
    if (size <= 0) return 0;
    if (length > (size_t)size) length = (size_t)size;
    if (length > 4096) length = 4096;
    memcpy(buffer, source->input + source->offset, length);
    source->offset += length;
    return (int)length;
}

static void append_detail(struct xml_error *failure, const char *text, size_t length) {
    if (failure->out_of_memory) {
        return;
    }
    if (length > SIZE_MAX - failure->length - 1) {
        failure->out_of_memory = 1;
        return;
    }
    char *output = xmlRealloc(failure->detail, failure->length + length + 1);
    if (output == NULL) {
        failure->out_of_memory = 1;
        return;
    }
    memcpy(output + failure->length, text, length);
    failure->length += length;
    output[failure->length] = '\0';
    failure->detail = output;
}

static void format_error(struct xml_error *failure, const xmlError *error) {
    if (failure->length != 0) {
        append_detail(failure, "\n", 1);
    }
    if (error->line > 0) {
        char line[64];
        int length = snprintf(line, sizeof(line), "line %d: ", error->line);
        if (length > 0 && (size_t)length < sizeof(line)) {
            append_detail(failure, line, (size_t)length);
        }
    }
    const char *message = error->message == NULL ? "(no message provided)" : error->message;
    size_t length = strlen(message);
    while (length != 0 && message[length - 1] == '\n') {
        length--;
    }
    append_detail(failure, message, length);
    const xmlChar *window;
    int size = 80;
    int offset;
    if (xmlCtxtGetInputWindow(failure->parser, -1, &window, &size, &offset) == 0) {
        append_detail(failure, "\n", 1);
        append_detail(failure, (const char *)window, (size_t)size);
        append_detail(failure, "\n", 1);
        for (int index = 0; index < offset; index++) {
            append_detail(failure, window[index] == '\t' ? "\t" : " ", 1);
        }
        append_detail(failure, "^", 1);
    }
}

static void record_error(void *context, const xmlError *error) {
    struct xml_error *failure = context;
    if (error->code == XML_ERR_NO_MEMORY) {
        failure->out_of_memory = 1;
        return;
    }
    if ((error->domain != XML_FROM_NONE && error->domain != XML_FROM_PARSER &&
         error->domain != XML_FROM_MEMORY && error->domain != XML_FROM_IO) ||
        error->code == XML_WAR_UNDECLARED_ENTITY ||
        (error->code == XML_ERR_NOT_WELL_BALANCED && failure->length != 0)) {
        return;
    }
    if (error->level >= XML_ERR_ERROR) {
        format_error(failure, error);
        return;
    }
    struct xml_error warning = {NULL, 0, 0, failure->parser, NULL, NULL};
    format_error(&warning, error);
    uqa_sql_xml_notice *notice = warning.out_of_memory ? NULL : xmlMalloc(sizeof(*notice));
    if (notice == NULL) {
        xmlFree(warning.detail);
        failure->out_of_memory = 1;
        return;
    }
    notice->message = warning.detail;
    notice->level = error->level;
    notice->next = NULL;
    if (failure->last_notice == NULL) {
        failure->notices = notice;
    } else {
        failure->last_notice->next = notice;
    }
    failure->last_notice = notice;
}

static xmlParserErrors empty_external_resource(void *context, const char *url,
                                               const char *public_id, xmlResourceType type,
                                               xmlParserInputFlags flags,
                                               xmlParserInput **output) {
    (void)context;
    (void)url;
    (void)public_id;
    (void)type;
    (void)flags;
    *output = xmlNewInputFromMemory(NULL, "", 0,
        XML_INPUT_BUF_STATIC | XML_INPUT_BUF_ZERO_TERMINATED);
    return *output == NULL ? XML_ERR_NO_MEMORY : XML_ERR_OK;
}

int uqa_sql_xml_validate(const char *input, size_t length, int document,
                         const char *version, size_t version_length,
                         char **detail, uqa_sql_xml_notice **notices) {
    xmlParserCtxt *context = xmlNewParserCtxt();
    struct xml_error failure = {NULL, 0, 0, context, NULL, NULL};
    xmlDoc *tree = NULL;
    xmlNode *nodes = NULL;
    struct xml_source input_source = {input, length, 0};
    int result = 0;
    *detail = NULL;
    *notices = NULL;
    if (context == NULL) {
        return 2;
    }
    xmlCtxtSetErrorHandler(context, record_error, &failure);
    xmlCtxtSetResourceLoader(context, empty_external_resource, NULL);
    xmlCtxtSetOptions(context, XML_PARSE_NOENT | XML_PARSE_DTDATTR);
    if (document) {
        if (length > INT_MAX) {
            result = 2;
        } else {
            tree = xmlCtxtReadIO(context, read_input, NULL, &input_source, NULL, "UTF-8",
                                XML_PARSE_NOENT | XML_PARSE_DTDATTR);
            if (tree == NULL) {
                result = 1;
            }
        }
    } else {
        xmlChar *parsed_version = version_length > INT_MAX ? NULL :
            xmlStrndup((const xmlChar *)version, (int)version_length);
        tree = parsed_version == NULL ? NULL : xmlNewDoc(parsed_version);
        xmlFree(parsed_version);
        if (tree == NULL) {
            result = 2;
        } else {
            xmlParserInput *source = xmlNewInputFromIO(NULL, read_input, NULL, &input_source, 0);
            if (source == NULL) {
                result = 2;
            } else {
                nodes = xmlCtxtParseContent(context, source, (xmlNode *)tree, 0);
            }
        }
    }
    if (result == 0 && (xmlCtxtGetStatus(context) &
         (XML_STATUS_NOT_WELL_FORMED | XML_STATUS_CATASTROPHIC_ERROR)) != 0) {
        result = 1;
    }
    if (failure.out_of_memory) {
        result = 2;
    }
    xmlFreeNodeList(nodes);
    xmlFreeDoc(tree);
    xmlFreeParserCtxt(context);
    *notices = failure.notices;
    if (result == 0) {
        xmlFree(failure.detail);
    } else {
        *detail = failure.detail;
    }
    return result;
}

void uqa_sql_xml_free_detail(char *detail) {
    xmlFree(detail);
}

int uqa_sql_xml_name_character(unsigned int character) {
    return xmlIsBaseCharQ(character) || xmlIsIdeographicQ(character) ||
        xmlIsDigitQ(character) || character == '.' || character == '-' ||
        character == '_' || character == ':' || xmlIsCombiningQ(character) ||
        xmlIsExtenderQ(character);
}

const char *uqa_sql_xml_notice_message(const uqa_sql_xml_notice *notice, int *level) {
    *level = notice->level;
    return notice->message;
}

const uqa_sql_xml_notice *uqa_sql_xml_notice_next(const uqa_sql_xml_notice *notice) {
    return notice->next;
}

void uqa_sql_xml_notices_free(uqa_sql_xml_notice *notices) {
    while (notices != NULL) {
        uqa_sql_xml_notice *next = notices->next;
        xmlFree(notices->message);
        xmlFree(notices);
        notices = next;
    }
}
