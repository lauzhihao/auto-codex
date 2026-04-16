# scodex

[English](./README.md) | [简体中文](./README.zh-CN.md)

`scodex` selects the Codex account with the best immediately usable quota, switches `~/.codex/auth.json`, and then launches or resumes Codex.

The repository is intentionally code-only. It does not contain account pool data, cached usage, local config, or virtualenv files.

The Rust binary is now the only maintained distribution target. The legacy Python implementation remains in the repository as a migration reference and is no longer the install path or the maintained runtime.

If you do not like or are not used to the command line, try the more feature-rich GUI version: <https://github.com/murongg/ai-accounts-hub>

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/lauzhihao/scodex/main/install.sh | bash
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/lauzhihao/scodex/main/install.ps1 | iex
```

Current prebuilt release targets:

- Linux: `x86_64-unknown-linux-musl`
- macOS: `x86_64-apple-darwin`, `aarch64-apple-darwin`
- Windows: `x86_64-pc-windows-msvc`

The installer:

- downloads a prebuilt Rust release binary from GitHub Releases
- installs `scodex` as the primary command
- keeps `auto-codex` as a compatibility command
- installs `scodex-original` as a thin passthrough helper to the underlying `codex`
- imports `~/.codex/auth.json` into local state when it exists
- refreshes usage cache after import when the usage API is reachable

## Requirements

- `codex`
- Unix installer: `bash`, `curl`, `tar`
- Windows installer: PowerShell 5+ or PowerShell 7+

Build from source:

```bash
cargo build --release
```

## Entrypoints

- `scodex`: primary command
- `auto-codex`: legacy compatibility wrapper
- `scodex-original`: passthrough helper to the underlying Codex CLI binary
- `codex`: the official Codex CLI command, left unchanged

## Command Overview

Use `scodex` as the default command. The legacy `auto-codex` wrapper is kept only for backward compatibility.

| Command | Purpose |
| --- | --- |
| `scodex` | Refresh usage, keep the current account when its 5h quota is at least 20%, otherwise switch to the best account, then launch or resume Codex |
| `scodex launch` | Explicit form of the default behavior |
| `scodex auto` | Refresh usage, keep the current account when its 5h quota is at least 20%, otherwise switch to the best account, without launching Codex |
| `scodex login` | Add one account via `codex login --device-auth` |
| `scodex use <email>` | Switch directly to a known account by email |
| `scodex list` | Refresh live usage, then show the latest account quotas |
| `scodex refresh` | Refresh live usage for all known accounts and print the latest results |
| `scodex import-auth <path>` | Import an `auth.json` file or a home directory containing `auth.json` |
| `scodex import-known` | Import `~/.codex/auth.json`; optionally import AI Accounts Hub managed homes |
| `scodex update` | Download the latest matching Rust release asset from GitHub Releases and replace the installed binary (`upgrade` is an alias) |

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
- after refresh, if the current account still has at least 20% remaining in the 5h window, `launch` keeps using it instead of re-scoring all accounts

### `auto`

```bash
scodex auto [--no-import-known] [--no-login] [--dry-run]
```

- refreshes usage and keeps the current account when its 5h quota is at least 20%; otherwise it switches to the best account
- does not start Codex

### `login`

```bash
scodex login [--switch]
```

- `--switch`: switch to the newly added account after login

### `use`

```bash
scodex use <email>
```

- switches directly to the known account whose email matches `<email>` case-insensitively
- example: `scodex use lauzhihao@qq.com`

### `list`

```bash
scodex list
```

- refreshes live usage first, then prints the latest account quota snapshot

### `refresh`

```bash
scodex refresh
```

- calls the live usage API for all known accounts
- prints the refreshed account list immediately after the API calls finish
- the current Rust release refreshes sequentially

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
scodex update [-f|--force]
scodex upgrade [-f|--force]
```

- downloads the latest matching GitHub Releases asset for the current platform and replaces the installed binary
- `update` remains the primary command for compatibility with the historical Python implementation
- `upgrade` is a compatible alias for users who prefer that wording
- `-f`, `--force`: force reinstall even when the current version already matches the latest release tag

## Passthrough Behavior

If the first non-global argument is not one of the documented subcommands, `scodex` treats it as a Codex CLI command after account selection.

Examples:

```bash
scodex resume --last
scodex exec "fix failing test"
```

This is why `scodex resume --last` works even though `resume` is not a declared `scodex` subcommand.

## Selection Notes

- usage refresh runs against the live usage API, not only the local cache
- account selection prefers higher `5h` remaining quota before weekly quota
- the goal is to choose the account most likely to be immediately usable for the next session

## Publish Checklist

Before pushing:

1. Run `rg -n 'access_token|refresh_token|id_token|OPENAI_API_KEY|account_id|@qq\\.com|/Users/ncds|/Users/liuzhihao' .`
2. Confirm `git status --short` only shows code and docs.
3. Review `git diff --cached` before pushing.

## Release Notes

- CI now targets the Rust implementation only.
- Tagged releases `v*` publish prebuilt binaries through GitHub Actions.
- The Python files in this repository are legacy reference code and are no longer maintained.
