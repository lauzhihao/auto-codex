# auto-codex

[English](./README.md) | [简体中文](./README.zh-CN.md)

`auto-codex` selects the Codex account with the best immediately usable quota, switches `~/.codex/auth.json`, and then launches or resumes Codex.

The repository is intentionally code-only. It does not contain account pool data, cached usage, local config, or virtualenv files.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/lauzhihao/auto-codex/main/install.sh | bash
```

The installer:

- downloads `codex-autoswitch.py` into `~/.local/share/auto-codex/`
- creates `~/.local/bin/scodex` as the primary command
- creates `~/.local/bin/auto-codex` as a compatibility command
- imports `~/.codex/auth.json` into local state when it exists
- refreshes usage cache after import when the usage API is reachable
- adds or updates a managed `alias scodex-original="..."` block in `~/.zshrc` and/or `~/.bashrc`
- does not alias `codex`, so the official Codex CLI command remains untouched

## Requirements

- `bash`
- `curl`
- `python3`
- `codex`

## Entrypoints

- `scodex`: primary command
- `auto-codex`: compatibility command with the same behavior
- `scodex-original`: alias to the underlying Codex CLI binary
- `codex`: the official Codex CLI command, left unchanged by the installer

## Command Overview

Use `scodex` as the default command. Every command below also works with `auto-codex`.

| Command | Purpose |
| --- | --- |
| `scodex` | Refresh usage, switch to the best account, then launch or resume Codex |
| `scodex launch` | Explicit form of the default behavior |
| `scodex auto` | Refresh usage, pick the best account, and switch without launching Codex |
| `scodex login` | Add one account via `codex login --device-auth` |
| `scodex list` | Show known accounts and cached usage |
| `scodex refresh` | Refresh live usage for all known accounts |
| `scodex import-auth <path>` | Import an `auth.json` file or a home directory containing `auth.json` |
| `scodex import-known` | Import `~/.codex/auth.json`; optionally import AI Accounts Hub managed homes |
| `scodex update` | Update `auto-codex` from its configured install source |

## Supported Options

### Global Options

- `--state-dir <path>`: override the local state directory
- `-h`, `--help`: show help

### `launch`

```bash
scodex launch [--no-import-known] [--no-login] [--dry-run] [--no-resume] [--no-launch] [<codex args...>]
```

- `--no-import-known`: skip auto-import of `~/.codex/auth.json`
- `--no-login`: do not start device auth when no usable account exists
- `--dry-run`: print which account would be selected without switching or launching
- `--no-resume`: always start a fresh Codex session instead of `resume --last`
- `--no-launch`: switch the account but do not start Codex
- extra args after the command are forwarded to Codex

### `auto`

```bash
scodex auto [--no-import-known] [--no-login] [--dry-run]
```

- refreshes usage and switches the selected account
- does not start Codex

### `login`

```bash
scodex login [--switch]
```

- `--switch`: switch to the newly added account after login

### `list`

```bash
scodex list
```

- shows known accounts and the locally cached usage snapshot
- does not refresh live usage by itself

### `refresh`

```bash
scodex refresh
```

- calls the live usage API for all known accounts
- refresh uses up to 8 parallel workers by default
- override worker count with `AUTO_CODEX_REFRESH_WORKERS`

### `import-auth`

```bash
scodex import-auth <path>
```

- `<path>` can be an `auth.json` file or a parent directory containing `auth.json`

### `import-known`

```bash
scodex import-known
```

- imports `~/.codex/auth.json`
- to also import AI Accounts Hub managed Codex homes, set:

```bash
AUTO_CODEX_IMPORT_ACCOUNTS_HUB=1 scodex import-known
```

### `update`

```bash
scodex update [--yes]
```

- updates the installed script and wrappers from the configured raw GitHub source
- `--yes`: skip installer confirmation prompts if they are added back in future revisions

## Passthrough Behavior

If the first non-global argument is not one of the documented subcommands, `scodex` treats it as a Codex CLI command after account selection.

Examples:

```bash
scodex resume --last
scodex exec "fix failing test"
```

This is why `scodex resume --last` works even though `resume` is not a declared `auto-codex` subcommand.

## Selection Notes

- usage refresh runs against the live usage API, not only the local cache
- account selection prefers higher `5h` remaining quota before weekly quota
- the goal is to choose the account most likely to be immediately usable for the next session

## Publish Checklist

Before pushing:

1. Run `rg -n 'access_token|refresh_token|id_token|OPENAI_API_KEY|account_id|@qq\\.com|/Users/ncds|/Users/liuzhihao' .`
2. Confirm `git status --short` only shows code and docs.
3. Review `git diff --cached` before pushing.
