#!/usr/bin/env bash
set -eo pipefail

install_manylinux_build_deps() {
  local libclang_dir="/opt/rh/llvm-toolset-7.0/root/usr/lib64"
  local -a packages=()

  if ! perl -MIPC::Cmd -MTime::Piece -e 1 2>/dev/null; then
    packages+=(perl-core)
  fi

  if [[ ! -f "$libclang_dir/libclang.so" ]]; then
    packages+=(llvm-toolset-7.0-clang-libs)
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

  export LIBCLANG_PATH="$libclang_dir"
  export LD_LIBRARY_PATH="$libclang_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

  if ldd "$libclang_dir/libclang.so" | grep -q "not found"; then
    echo "libclang has unresolved shared-library dependencies" >&2
    return 1
  fi
}

install_manylinux_build_deps
unset -f install_manylinux_build_deps
