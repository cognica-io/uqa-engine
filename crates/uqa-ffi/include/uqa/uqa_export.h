/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_EXPORT_H
#define UQA_EXPORT_H

#if defined(UQA_BUILD_SHARED) && defined(UQA_USE_SHARED)
#error "UQA_BUILD_SHARED and UQA_USE_SHARED are mutually exclusive"
#endif

#if defined(_WIN32) || defined(__CYGWIN__)
#define UQA_CALL __cdecl
#else
#define UQA_CALL
#endif

#if !defined(UQA_BUILD_SHARED) && !defined(UQA_USE_SHARED)
#define UQA_API
#elif defined(_WIN32) || defined(__CYGWIN__)
#if defined(UQA_BUILD_SHARED)
#define UQA_API __declspec(dllexport)
#else
#define UQA_API __declspec(dllimport)
#endif
#elif defined(__GNUC__) || defined(__clang__)
#define UQA_API __attribute__((visibility("default")))
#else
#error "UQA shared-library visibility requires a supported compiler"
#endif

#endif /* UQA_EXPORT_H */
