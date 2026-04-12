#!/usr/bin/env bash
set -euo pipefail

RAW_BASE="${AUTO_CODEX_RAW_BASE:-https://raw.githubusercontent.com/lauzhihao/auto-codex/main}"
INSTALL_BIN="${HOME}/.local/bin"
INSTALL_HOME="${HOME}/.local/share/auto-codex"
SCRIPT_URL="${RAW_BASE}/codex-autoswitch.py"
SCRIPT_PATH="${INSTALL_HOME}/codex-autoswitch.py"
WRAPPER_PATH="${INSTALL_BIN}/auto-codex"

if [[ "${RAW_BASE}" == *"<your-name>"* || "${RAW_BASE}" == *"<repo>"* ]]; then
  echo "Replace the placeholder GitHub repo in install.sh before publishing, or set AUTO_CODEX_RAW_BASE." >&2
  exit 1
fi

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

need_cmd bash
need_cmd curl
need_cmd python3
need_cmd codex

REAL_CODEX_BIN="$(command -v codex)"

append_shell_block() {
  local rc_file="$1"
  local begin_marker="# >>> auto-codex >>>"
  local end_marker="# <<< auto-codex <<<"

  mkdir -p "$(dirname "${rc_file}")"
  touch "${rc_file}"

  if grep -Fq "${begin_marker}" "${rc_file}"; then
    return
  fi

  {
    printf '\n%s\n' "${begin_marker}"
    printf 'export PATH="$HOME/.local/bin:$PATH"\n'
    printf 'alias codex="auto-codex"\n'
    printf "alias codex-original='%s'\n" "${REAL_CODEX_BIN}"
    printf '%s\n' "${end_marker}"
  } >> "${rc_file}"
}

select_rc_files() {
  local shell_name
  shell_name="$(basename "${SHELL:-}")"

  if [[ -f "${HOME}/.zshrc" || "${shell_name}" == "zsh" ]]; then
    printf '%s\n' "${HOME}/.zshrc"
  fi
  if [[ -f "${HOME}/.bashrc" || "${shell_name}" == "bash" ]]; then
    printf '%s\n' "${HOME}/.bashrc"
  fi
}

mkdir -p "${INSTALL_BIN}" "${INSTALL_HOME}"

tmp_script="$(mktemp "${TMPDIR:-/tmp}/auto-codex.XXXXXX.py")"
trap 'rm -f "${tmp_script}"' EXIT

curl -fsSL "${SCRIPT_URL}" -o "${tmp_script}"
install -m 0755 "${tmp_script}" "${SCRIPT_PATH}"

cat > "${WRAPPER_PATH}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export AUTO_CODEX_HOME="${HOME}/.local/share/auto-codex"
export AUTO_CODEX_PROG="auto-codex"
exec python3 "${AUTO_CODEX_HOME}/codex-autoswitch.py" "$@"
EOF

chmod 0755 "${WRAPPER_PATH}"

while IFS= read -r rc_file; do
  [[ -n "${rc_file}" ]] || continue
  append_shell_block "${rc_file}"
done < <(select_rc_files)

if [[ -f "${HOME}/.codex/auth.json" ]]; then
  if "${WRAPPER_PATH}" import-known >/dev/null; then
    echo "Imported ${HOME}/.codex/auth.json into auto-codex."
    if "${WRAPPER_PATH}" refresh >/dev/null; then
      echo "Refreshed auto-codex usage cache."
    else
      echo "Imported ${HOME}/.codex/auth.json, but refreshing usage cache failed." >&2
    fi
  else
    echo "Install succeeded, but importing ${HOME}/.codex/auth.json failed." >&2
  fi
else
  echo "No ${HOME}/.codex/auth.json found; skipped import."
fi

echo "Installed to ${WRAPPER_PATH}"
echo "Added shell aliases in ~/.zshrc and/or ~/.bashrc:"
echo "  codex -> auto-codex"
echo "  codex-original -> ${REAL_CODEX_BIN}"
echo "Open a new shell or source your shell rc file to activate the codex alias."
