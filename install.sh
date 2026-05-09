#!/usr/bin/env bash
set -euo pipefail

REPO="${AUTO_CODEX_REPO:-lauzhihao/scodex}"
SCODEX_HOME="${SCODEX_HOME:-$HOME/.scodex}"
BIN_DIR="${SCODEX_HOME}/bin"
SHIM_PATH="${HOME}/.local/bin/scodex"
COMPAT_SHIM_PATH="${HOME}/.local/bin/auto-codex"
ORIGINAL_WRAPPER_PATH="${HOME}/.local/bin/scodex-original"
VERSION="${AUTO_CODEX_VERSION:-}"

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

show_requirements() {
  local missing=0
  local cmd
  echo "Dependency check:"
  for cmd in bash curl tar mktemp; do
    if need_cmd "${cmd}"; then
      printf '  [ok] %s -> %s\n' "${cmd}" "$(command -v "${cmd}")"
    else
      printf '  [missing] %s\n' "${cmd}" >&2
      missing=1
    fi
  done
  if [[ "${missing}" -ne 0 ]]; then
    echo "Install aborted because required commands are missing." >&2
    exit 1
  fi
}

detect_target() {
  local os arch
  os="$(uname -s 2>/dev/null || echo unknown)"
  arch="$(uname -m 2>/dev/null || echo unknown)"

  case "${os}/${arch}" in
    Darwin/arm64|Darwin/aarch64)
      echo "aarch64-apple-darwin"
      ;;
    Darwin/x86_64)
      echo "x86_64-apple-darwin"
      ;;
    Linux/x86_64|Linux/amd64)
      echo "x86_64-unknown-linux-musl"
      ;;
    *)
      echo "Unsupported platform: ${os}/${arch}" >&2
      echo "Use a published release asset manually or build from source with cargo." >&2
      exit 1
      ;;
  esac
}

is_wsl_environment() {
  if [[ -n "${WSL_INTEROP:-}" || -n "${WSL_DISTRO_NAME:-}" ]]; then
    return 0
  fi

  if [[ -r /proc/sys/kernel/osrelease ]] \
    && grep -qiE 'microsoft|wsl' /proc/sys/kernel/osrelease; then
    return 0
  fi

  if [[ -r /proc/version ]] && grep -qiE 'microsoft|wsl' /proc/version; then
    return 0
  fi

  return 1
}

is_windows_interop_command() {
  local path
  path="$1"
  case "${path}" in
    /mnt/[a-zA-Z]/*|/mnt/[a-zA-Z]\\*|*.cmd|*.CMD|*.bat|*.BAT|*.exe|*.EXE)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

warn_wsl_runtime_paths() {
  local cmd path found_windows_path=0

  if ! is_wsl_environment; then
    return 0
  fi

  echo "WSL environment detected; installing the Linux/WSL scodex binary."
  echo "scodex will use WSL-local state under ${SCODEX_HOME} and ${HOME}/.codex."

  for cmd in codex npm; do
    path="$(command -v "${cmd}" 2>/dev/null || true)"
    if [[ -n "${path}" ]] && is_windows_interop_command "${path}"; then
      echo "Warning: ${cmd} resolves to a Windows PATH entry: ${path}" >&2
      found_windows_path=1
    fi
  done

  if [[ "${found_windows_path}" -ne 0 ]]; then
    echo "Install the Linux versions inside WSL, or put WSL-local bin directories before Windows PATH entries." >&2
    echo "The scodex runtime skips Windows interop codex/npm paths when running under WSL." >&2
  fi
}

resolve_version() {
  if [[ -n "${VERSION}" ]]; then
    echo "${VERSION}"
    return 0
  fi

  local api_url
  api_url="https://api.github.com/repos/${REPO}/releases/latest"
  VERSION="$(
    curl -fsSL "${api_url}" \
      | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
      | head -n 1
  )"
  if [[ -z "${VERSION}" ]]; then
    echo "Failed to resolve latest release tag from ${api_url}" >&2
    exit 1
  fi
  echo "${VERSION}"
}

download_and_install() {
  local version target asset url tmp_dir cleanup_dir archive_path extracted_path
  version="$1"
  target="$2"
  asset="scodex-${version}-${target}.tar.gz"
  url="https://github.com/${REPO}/releases/download/${version}/${asset}"
  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/scodex-install.XXXXXX")"
  cleanup_dir="${tmp_dir}"
  trap 'rm -rf -- "'"${cleanup_dir}"'"' EXIT
  archive_path="${tmp_dir}/${asset}"

  echo "Downloading ${url}"
  curl -fsSL "${url}" -o "${archive_path}"

  mkdir -p "${BIN_DIR}"
  tar -xzf "${archive_path}" -C "${tmp_dir}"
  extracted_path="${tmp_dir}/scodex"
  if [[ ! -f "${extracted_path}" ]]; then
    echo "Release archive did not contain a top-level scodex binary." >&2
    exit 1
  fi

  install -m 0755 "${extracted_path}" "${BIN_DIR}/scodex"
  cp "${BIN_DIR}/scodex" "${BIN_DIR}/auto-codex"
}

install_shim_scripts() {
  mkdir -p "${HOME}/.local/bin"

  cat > "${SHIM_PATH}" <<'EOF'
#!/usr/bin/env bash
# scodex shim v1
SCODEX_HOME="${SCODEX_HOME:-$HOME/.scodex}"
exec "$SCODEX_HOME/bin/scodex" "$@"
EOF
  chmod 0755 "${SHIM_PATH}"

  cp "${SHIM_PATH}" "${COMPAT_SHIM_PATH}"
}

install_original_wrapper() {
  mkdir -p "${HOME}/.local/bin"
  cat > "${ORIGINAL_WRAPPER_PATH}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if command -v codex >/dev/null 2>&1; then
  exec "$(command -v codex)" "$@"
fi
echo "codex not found on PATH." >&2
exit 1
EOF
  chmod 0755 "${ORIGINAL_WRAPPER_PATH}"
}

post_install_import() {
  if [[ -f "${HOME}/.codex/auth.json" ]]; then
    if "${BIN_DIR}/scodex" import-known >/dev/null 2>&1; then
      echo "Imported ${HOME}/.codex/auth.json into scodex state."
      if "${BIN_DIR}/scodex" refresh >/dev/null 2>&1; then
        echo "Refreshed scodex usage cache."
      else
        echo "Imported auth.json, but refreshing usage cache failed." >&2
      fi
    else
      echo "Installed scodex, but importing ${HOME}/.codex/auth.json failed." >&2
    fi
  else
    echo "No ${HOME}/.codex/auth.json found; skipped import."
  fi
}

print_next_steps() {
  echo "Installed binary to ${BIN_DIR}/scodex"
  echo "Installed shim to ${SHIM_PATH}"
  echo "Installed compatibility command to ${COMPAT_SHIM_PATH}"
  echo "Installed passthrough helper to ${ORIGINAL_WRAPPER_PATH}"
  if [[ ":$PATH:" != *":${HOME}/.local/bin:"* ]]; then
    echo
    echo "${HOME}/.local/bin is not currently on PATH."
    echo "Add this line to your shell profile:"
    echo "  export PATH=\"${HOME}/.local/bin:\$PATH\""
  fi
}

show_requirements
TARGET="$(detect_target)"
warn_wsl_runtime_paths
VERSION="$(resolve_version)"
download_and_install "${VERSION}" "${TARGET}"
install_shim_scripts
install_original_wrapper
post_install_import
print_next_steps
