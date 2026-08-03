#!/usr/bin/env bash
set -eo pipefail

install_manylinux_build_deps() {
  local libclang_dir="/usr/lib64"
  local libclang_libraries="$libclang_dir/libclang.so*"
  local clang_resource_headers="/usr/lib/clang/*/include/stddef.h"
  local libclang_library
  local clang_resource_dir
  local -a libclang_library_paths=()
  local -a clang_resource_header_paths=()
  local -a packages=()

  if ! perl -MIPC::Cmd -MTime::Piece -e 1 2>/dev/null; then
    packages+=(perl-core)
  fi

  if ! compgen -G "$libclang_libraries" >/dev/null ||
    ! compgen -G "$clang_resource_headers" >/dev/null; then
    packages+=(clang-libs)
  fi

  if (( ${#packages[@]} > 0 )); then
    if command -v dnf >/dev/null 2>&1; then
      dnf install -y "${packages[@]}"
    elif command -v yum >/dev/null 2>&1; then
      yum install -y "${packages[@]}"
    else
      echo "No supported package manager is available for manylinux build dependencies" >&2
      return 1
    fi
  fi

  perl -MIPC::Cmd -MTime::Piece -e 1
  mapfile -t libclang_library_paths < <(compgen -G "$libclang_libraries")
  mapfile -t clang_resource_header_paths < <(compgen -G "$clang_resource_headers")
  (( ${#libclang_library_paths[@]} > 0 ))
  (( ${#clang_resource_header_paths[@]} > 0 ))
  libclang_library="${libclang_library_paths[0]}"
  clang_resource_dir="${clang_resource_header_paths[0]%/include/stddef.h}"

  export LIBCLANG_PATH="$libclang_dir"
  export LD_LIBRARY_PATH="$libclang_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export BINDGEN_EXTRA_CLANG_ARGS="-resource-dir=$clang_resource_dir${BINDGEN_EXTRA_CLANG_ARGS:+ $BINDGEN_EXTRA_CLANG_ARGS}"

  if ldd "$libclang_library" | grep -q "not found"; then
    echo "libclang has unresolved shared-library dependencies" >&2
    return 1
  fi
}

install_manylinux_build_deps
unset -f install_manylinux_build_deps
