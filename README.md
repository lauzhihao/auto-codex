# auto-codex

`auto-codex` picks the Codex account with the highest remaining quota, switches `~/.codex/auth.json`, and then launches Codex.

The repository is intentionally code-only. It does not contain any account pool data, cached usage, local config, or virtualenv files.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/lauzhihao/auto-codex/main/install.sh | bash
```

The installer prints a step-by-step execution plan, waits for a `Y/N` confirmation, and then runs.
For non-interactive installs, set `AUTO_CODEX_YES=1`.
If required dependencies are missing, the installer prints environment details and suggested install commands, then exits without changing the machine.

The installer:

- downloads `codex-autoswitch.py` into `~/.local/share/auto-codex/`
- creates `~/.local/bin/auto-codex`
- imports `~/.codex/auth.json` into `auto-codex` state when it exists
- refreshes usage cache after import when the usage API is reachable
- adds or updates a managed `alias codex="auto-codex"` block in `~/.zshrc` and/or `~/.bashrc`
- adds `alias codex-original="..."` as a direct escape hatch to the real Codex CLI
- keeps all runtime state on the local machine

## Requirements

- `bash`
- `curl`
- `python3`
- `codex`

## Usage

```bash
codex
codex update
codex update --yes
codex resume --last
auto-codex list
codex-original --help
```

`codex update` updates `auto-codex` itself from the configured install source.
Use `codex-original` if you need the underlying Codex CLI directly.

## Publish Checklist

Before the first push:

1. Run `rg -n 'access_token|refresh_token|id_token|OPENAI_API_KEY|account_id|@qq\\.com|/Users/ncds|/Users/liuzhihao' .`
2. Confirm `git status --short` only shows code and docs.
3. Review `git diff --cached` before pushing.
