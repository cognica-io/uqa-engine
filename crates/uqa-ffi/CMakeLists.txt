#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

cmake_minimum_required(VERSION 3.24)
project(UQA LANGUAGES C)
if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
  message(FATAL_ERROR "The C distribution supports 64-bit targets only")
endif()
include(GNUInstallDirs)
include(CMakePackageConfigHelpers)
find_package(Python3 3.11 REQUIRED COMPONENTS Interpreter)

# Cargo owns compilation and dependency selection. This project links the native
# distribution from that exact archive with a platform-specific ABI export list.
set(UQA_RUST_STATIC_LIBRARY "" CACHE FILEPATH "Cargo staticlib-only uqa_ffi archive")
set(UQA_NATIVE_LINK_FLAGS "" CACHE STRING "Exact rustc --print=native-static-libs output")
set(UQA_SOURCE_REVISION "" CACHE STRING "Source revision embedded in the Rust library")
set(UQA_FEATURES "" CACHE STRING "Explicit comma-separated enabled Cargo features")
set(UQA_TARGET "" CACHE STRING "Rust target triple of the supplied archive")
set(UQA_CARGO_METADATA "" CACHE FILEPATH "cargo metadata JSON for the same target and features")
if(NOT EXISTS "${UQA_RUST_STATIC_LIBRARY}")
  message(FATAL_ERROR "Supply the Cargo-built archive as UQA_RUST_STATIC_LIBRARY")
endif()
if(NOT UQA_NATIVE_LINK_FLAGS OR NOT UQA_SOURCE_REVISION OR NOT UQA_TARGET)
  message(FATAL_ERROR "Supply UQA_NATIVE_LINK_FLAGS, UQA_SOURCE_REVISION and UQA_TARGET")
endif()
if(NOT EXISTS "${UQA_CARGO_METADATA}")
  message(FATAL_ERROR "Supply the matching Cargo dependency graph as UQA_CARGO_METADATA")
endif()
get_filename_component(UQA_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
execute_process(
  COMMAND "${Python3_EXECUTABLE}" "${UQA_ROOT}/scripts/c-bindings-metadata.py"
    --output "${CMAKE_CURRENT_BINARY_DIR}/metadata" --target "${UQA_TARGET}"
    --revision "${UQA_SOURCE_REVISION}" --features "${UQA_FEATURES}"
    --cargo-metadata "${UQA_CARGO_METADATA}"
    "--minimum-os=${CMAKE_OSX_DEPLOYMENT_TARGET}"
    "--native-link-flags=${UQA_NATIVE_LINK_FLAGS}"
  COMMAND_ERROR_IS_FATAL ANY)
include("${CMAKE_CURRENT_BINARY_DIR}/metadata/build.cmake")

add_library(uqa SHARED cmake/link.c)
add_library(UQA::uqa ALIAS uqa)
set_target_properties(uqa PROPERTIES VERSION "${UQA_VERSION}" SOVERSION 1
  C_VISIBILITY_PRESET hidden EXPORT_NAME uqa
  MACOSX_RPATH ON INSTALL_NAME_DIR "@rpath"
  MACHO_COMPATIBILITY_VERSION 1.0.0 MACHO_CURRENT_VERSION 1.0.0)
target_compile_definitions(uqa PRIVATE UQA_BUILD_SHARED INTERFACE UQA_USE_SHARED)
target_include_directories(uqa PUBLIC
  "$<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>"
  "$<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>")
target_link_libraries(uqa PRIVATE
  "$<LINK_LIBRARY:WHOLE_ARCHIVE,${UQA_RUST_STATIC_LIBRARY}>" ${UQA_NATIVE_LIBRARIES})
if(APPLE)
  target_link_options(uqa PRIVATE
    "LINKER:-exported_symbols_list,${CMAKE_CURRENT_BINARY_DIR}/metadata/exports.macho")
elseif(WIN32)
  if(NOT MSVC)
    message(FATAL_ERROR "The Windows C package requires the MSVC ABI")
  endif()
  target_sources(uqa PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/metadata/exports.def")
else()
  target_link_options(uqa PRIVATE
    "LINKER:--version-script=${CMAKE_CURRENT_BINARY_DIR}/metadata/exports.elf"
    "LINKER:--no-undefined")
endif()
set_property(TARGET uqa APPEND PROPERTY LINK_DEPENDS
  "${CMAKE_CURRENT_BINARY_DIR}/metadata/exports.macho"
  "${CMAKE_CURRENT_BINARY_DIR}/metadata/exports.elf"
  "${CMAKE_CURRENT_BINARY_DIR}/metadata/exports.def")

install(TARGETS uqa EXPORT UQATargets
  RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
  LIBRARY DESTINATION "${CMAKE_INSTALL_LIBDIR}"
  ARCHIVE DESTINATION "${CMAKE_INSTALL_LIBDIR}")
if(WIN32)
  set(UQA_STATIC_FILENAME uqa_static.lib)
else()
  set(UQA_STATIC_FILENAME libuqa.a)
endif()
install(FILES "${UQA_RUST_STATIC_LIBRARY}"
  DESTINATION "${CMAKE_INSTALL_LIBDIR}" RENAME "${UQA_STATIC_FILENAME}")
install(DIRECTORY include/uqa DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}")
install(EXPORT UQATargets NAMESPACE UQA:: DESTINATION "${CMAKE_INSTALL_LIBDIR}/cmake/UQA")
configure_package_config_file(cmake/UQAConfig.cmake.in
  "${CMAKE_CURRENT_BINARY_DIR}/UQAConfig.cmake"
  INSTALL_DESTINATION "${CMAKE_INSTALL_LIBDIR}/cmake/UQA"
  PATH_VARS CMAKE_INSTALL_LIBDIR CMAKE_INSTALL_INCLUDEDIR)
write_basic_package_version_file("${CMAKE_CURRENT_BINARY_DIR}/UQAConfigVersion.cmake"
  VERSION "${UQA_VERSION}" COMPATIBILITY SameMajorVersion)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/UQAConfig.cmake"
  "${CMAKE_CURRENT_BINARY_DIR}/UQAConfigVersion.cmake"
  DESTINATION "${CMAKE_INSTALL_LIBDIR}/cmake/UQA")
configure_file(cmake/uqa.pc.in "${CMAKE_CURRENT_BINARY_DIR}/uqa.pc" @ONLY)
configure_file(cmake/uqa-static.pc.in "${CMAKE_CURRENT_BINARY_DIR}/uqa-static.pc" @ONLY)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/uqa.pc" "${CMAKE_CURRENT_BINARY_DIR}/uqa-static.pc"
  DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig")
install(DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/metadata/share/"
  DESTINATION "${CMAKE_INSTALL_DATADIR}")
