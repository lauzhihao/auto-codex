# auto-codex

`auto-codex` picks the Codex account with the best remaining quota for immediate use, switches `~/.codex/auth.json`, and then launches Codex.

The repository is intentionally code-only. It does not contain any account pool data, cached usage, local config, or virtualenv files.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/lauzhihao/auto-codex/main/install.sh | bash
```

The installer prints a step-by-step execution plan and then runs immediately.
If required dependencies are missing, the installer prints environment details and suggested install commands, then exits without changing the machine.

The installer:

- downloads `codex-autoswitch.py` into `~/.local/share/auto-codex/`
- creates `~/.local/bin/scodex`
- imports `~/.codex/auth.json` into `auto-codex` state when it exists
- refreshes usage cache after import when the usage API is reachable
- adds or updates a managed `alias scodex-original="..."` block in `~/.zshrc` and/or `~/.bashrc`
- keeps all runtime state on the local machine

## Requirements

- `bash`
- `curl`
- `python3`
- `codex`

## Usage

```bash
scodex
scodex update
scodex update --yes
scodex resume --last
scodex list
scodex-original --help
```

`scodex update` updates `auto-codex` itself from the configured install source.
Use `scodex-original` if you need the underlying Codex CLI directly.

## Notes

- Account refresh runs against the live usage API, not only the local cache.
- Refresh uses up to 8 parallel workers by default. Override with `AUTO_CODEX_REFRESH_WORKERS`.
- Account selection prefers higher `5h` remaining quota before weekly quota so the chosen account is more likely to be immediately usable.
- To also import AI Accounts Hub managed Codex homes, set `AUTO_CODEX_IMPORT_ACCOUNTS_HUB=1` before running `scodex import-known` or `scodex`.

## Publish Checklist

Before the first push:

1. Run `rg -n 'access_token|refresh_token|id_token|OPENAI_API_KEY|account_id|@qq\\.com|/Users/ncds|/Users/liuzhihao' .`
2. Confirm `git status --short` only shows code and docs.
3. Review `git diff --cached` before pushing.
