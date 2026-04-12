#!/usr/bin/env bash
set -euo pipefail

RAW_BASE="${AUTO_CODEX_RAW_BASE:-https://raw.githubusercontent.com/lauzhihao/auto-codex/main}"
INSTALL_BIN="${HOME}/.local/bin"
INSTALL_HOME="${HOME}/.local/share/auto-codex"
SCRIPT_URL="${RAW_BASE}/codex-autoswitch.py"
SCRIPT_PATH="${INSTALL_HOME}/codex-autoswitch.py"
WRAPPER_PATH="${INSTALL_BIN}/auto-codex"
BEGIN_MARKER="# >>> auto-codex >>>"
END_MARKER="# <<< auto-codex <<<"

if [[ "${RAW_BASE}" == *"<your-name>"* || "${RAW_BASE}" == *"<repo>"* ]]; then
  echo "Replace the placeholder GitHub repo in install.sh before publishing, or set AUTO_CODEX_RAW_BASE." >&2
  exit 1
fi

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

detect_platform() {
  local os arch
  os="$(uname -s 2>/dev/null || echo unknown)"
  arch="$(uname -m 2>/dev/null || echo unknown)"
  printf '%s/%s' "${os}" "${arch}"
}

show_runtime_environment() {
  local shell_name
  shell_name="$(basename "${SHELL:-unknown}")"
  cat <<EOF
Runtime environment:
  platform: $(detect_platform)
  shell: ${shell_name}
  home: ${HOME}
  raw base: ${RAW_BASE}
EOF
}

print_install_hint() {
  local cmd="$1"
  local os
  os="$(uname -s 2>/dev/null || echo unknown)"

  case "${cmd}" in
    bash)
      case "${os}" in
        Darwin)
          echo "    install hint: bash is expected to exist on macOS. Verify /bin/bash is available."
          ;;
        Linux)
          echo "    install hint: ubuntu/debian: sudo apt-get update && sudo apt-get install -y bash"
          echo "    install hint: centos/rhel: sudo yum install -y bash"
          ;;
      esac
      ;;
    curl)
      case "${os}" in
        Darwin)
          echo "    install hint: curl is expected to exist on macOS. Verify /usr/bin/curl is available."
          ;;
        Linux)
          echo "    install hint: ubuntu/debian: sudo apt-get update && sudo apt-get install -y curl"
          echo "    install hint: centos/rhel: sudo yum install -y curl"
          ;;
      esac
      ;;
    python3)
      case "${os}" in
        Darwin)
          echo "    install hint: brew install python"
          ;;
        Linux)
          echo "    install hint: ubuntu/debian: sudo apt-get update && sudo apt-get install -y python3"
          echo "    install hint: centos/rhel: sudo yum install -y python3"
          ;;
      esac
      ;;
    codex)
      case "${os}" in
        Darwin)
          echo "    install hint: install Node.js first, then npm install -g @openai/codex"
          echo "    install hint: Homebrew Node.js: brew install node"
          ;;
        Linux)
          echo "    install hint: install Node.js first, then npm install -g @openai/codex"
          echo "    install hint: ubuntu/debian Node.js example: sudo apt-get update && sudo apt-get install -y nodejs npm"
          ;;
      esac
      ;;
  esac
}

show_plan() {
  cat <<EOF
auto-codex install plan

The script will perform these actions:
1. Check runtime environment and required commands: bash, curl, python3, codex.
2. Download:
   curl -fsSL ${SCRIPT_URL}
3. Install files:
   - ${SCRIPT_PATH}
   - ${WRAPPER_PATH}
4. Update managed shell config blocks in:
$(select_rc_files | sed 's/^/   - /')
5. If ${HOME}/.codex/auth.json exists:
   - ${WRAPPER_PATH} import-known
   - ${WRAPPER_PATH} refresh

The script will not modify ${HOME}/.codex/auth.json.
EOF
}

show_requirements() {
  local missing=0
  local cmd
  show_runtime_environment
  echo "Dependency check:"
  for cmd in bash curl python3 codex; do
    if need_cmd "${cmd}"; then
      printf '  [ok] %s -> %s\n' "${cmd}" "$(command -v "${cmd}")"
    else
      printf '  [missing] %s\n' "${cmd}" >&2
      print_install_hint "${cmd}" >&2
      missing=1
    fi
  done
  if [[ "${missing}" -ne 0 ]]; then
    echo "Install aborted because required commands are missing. Install them first, then re-run this script." >&2
    exit 1
  fi
}

confirm_install() {
  local reply
  if [[ "${AUTO_CODEX_YES:-}" == "1" || "${AUTO_CODEX_YES:-}" == "true" ]]; then
    return
  fi
  if ! tty -s >/dev/null 2>&1; then
    echo "Install requires confirmation. Re-run with AUTO_CODEX_YES=1 for non-interactive use." >&2
    exit 1
  fi
  printf 'Proceed with auto-codex install? [y/N] ' > /dev/tty
  read -r reply < /dev/tty || exit 1
  case "${reply}" in
    y|Y|yes|YES)
      return
      ;;
    *)
      echo "Install cancelled."
      exit 1
      ;;
  esac
}

REAL_CODEX_BIN=""

render_shell_block() {
  cat <<EOF
${BEGIN_MARKER}
export PATH="\$HOME/.local/bin:\$PATH"
alias codex="auto-codex"
alias codex-original='${REAL_CODEX_BIN}'
${END_MARKER}
EOF
}

upsert_shell_block() {
  local rc_file="$1"
  local status

  mkdir -p "$(dirname "${rc_file}")"
  touch "${rc_file}"

  status="$(
    python3 - "${rc_file}" "${BEGIN_MARKER}" "${END_MARKER}" "$(render_shell_block)" <<'PY'
from pathlib import Path
import sys

rc_path = Path(sys.argv[1])
begin = sys.argv[2]
end = sys.argv[3]
block = sys.argv[4].rstrip("\n")

text = rc_path.read_text(encoding="utf-8")
start = text.find(begin)
if start == -1:
    if text and not text.endswith("\n"):
        text += "\n"
    if text and not text.endswith("\n\n"):
        text += "\n"
    text += block + "\n"
    rc_path.write_text(text, encoding="utf-8")
    print("added")
    raise SystemExit(0)

finish = text.find(end, start)
if finish == -1:
    raise SystemExit(f"Managed block start found in {rc_path}, but end marker is missing.")

finish += len(end)
updated = text[:start].rstrip("\n") + "\n" + block + text[finish:]
if updated.startswith("\n"):
    updated = updated[1:]
if updated == text:
    print("unchanged")
else:
    rc_path.write_text(updated, encoding="utf-8")
    print("updated")
PY
  )"
  printf '  shell config %s: %s\n' "${status}" "${rc_file}"
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

show_requirements
REAL_CODEX_BIN="$(command -v codex)"
show_plan
confirm_install

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
export AUTO_CODEX_RAW_BASE="__AUTO_CODEX_RAW_BASE__"
exec python3 "${AUTO_CODEX_HOME}/codex-autoswitch.py" "$@"
EOF

python3 - "${WRAPPER_PATH}" "${RAW_BASE}" <<'PY'
from pathlib import Path
import sys

wrapper_path = Path(sys.argv[1])
raw_base = sys.argv[2]
text = wrapper_path.read_text(encoding="utf-8")
wrapper_path.write_text(text.replace("__AUTO_CODEX_RAW_BASE__", raw_base), encoding="utf-8")
PY

chmod 0755 "${WRAPPER_PATH}"

while IFS= read -r rc_file; do
  [[ -n "${rc_file}" ]] || continue
  upsert_shell_block "${rc_file}"
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
