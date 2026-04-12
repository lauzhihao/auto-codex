# auto-codex

`auto-codex` picks the Codex account with the highest remaining quota, switches `~/.codex/auth.json`, and then launches Codex.

The repository is intentionally code-only. It does not contain any account pool data, cached usage, local config, or virtualenv files.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/lauzhihao/auto-codex/main/install.sh | bash
```

The installer:

- downloads `codex-autoswitch.py` into `~/.local/share/auto-codex/`
- creates `~/.local/bin/auto-codex`
- imports `~/.codex/auth.json` into `auto-codex` state when it exists
- refreshes usage cache after import when the usage API is reachable
- adds `alias codex="auto-codex"` to `~/.zshrc` and/or `~/.bashrc`
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
codex resume --last
auto-codex list
codex-original --help
```

## Publish Checklist

Before the first push:

1. Run `rg -n 'access_token|refresh_token|id_token|OPENAI_API_KEY|account_id|@qq\\.com|/Users/ncds|/Users/liuzhihao' .`
2. Confirm `git status --short` only shows code and docs.
3. Review `git diff --cached` before pushing.
