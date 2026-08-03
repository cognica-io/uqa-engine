#!/usr/bin/env bash
set -eo pipefail

install_manylinux_build_deps() {
  local clang_root="/opt/rh/llvm-toolset-7.0/root/usr"
  local libclang_dir="$clang_root/lib64"
  local clang_resource_headers="$libclang_dir/clang/*/include/stddef.h"
  local clang_resource_dir
  local -a clang_resource_header_paths=()
  local -a packages=()

  if ! perl -MIPC::Cmd -MTime::Piece -e 1 2>/dev/null; then
    packages+=(perl-core)
  fi

  if [[ ! -f "$libclang_dir/libclang.so" ]] ||
    ! compgen -G "$clang_resource_headers" >/dev/null; then
    packages+=(llvm-toolset-7.0-clang)
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
  [[ -f "$libclang_dir/libclang.so" ]]
  mapfile -t clang_resource_header_paths < <(compgen -G "$clang_resource_headers")
  (( ${#clang_resource_header_paths[@]} > 0 ))
  clang_resource_dir="${clang_resource_header_paths[0]%/include/stddef.h}"

  export LIBCLANG_PATH="$libclang_dir"
  export LD_LIBRARY_PATH="$libclang_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export BINDGEN_EXTRA_CLANG_ARGS="-resource-dir=$clang_resource_dir${BINDGEN_EXTRA_CLANG_ARGS:+ $BINDGEN_EXTRA_CLANG_ARGS}"

  if ldd "$libclang_dir/libclang.so" | grep -q "not found"; then
    echo "libclang has unresolved shared-library dependencies" >&2
    return 1
  fi
}

install_manylinux_build_deps
unset -f install_manylinux_build_deps
