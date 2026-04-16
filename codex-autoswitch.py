#!/usr/bin/env python3
from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path


STATE_VERSION = 1
DEFAULT_USAGE_BASE_URL = "https://chatgpt.com/backend-api"
DEFAULT_STATE_BASENAME = "auto-codex"
LEGACY_STATE_BASENAME = "codex-autoswitch"
DEFAULT_INSTALL_BASE_URL = "https://raw.githubusercontent.com/lauzhihao/scodex/main"
DEFAULT_PROGRAM_NAME = "scodex"
KNOWN_COMMANDS = {"launch", "auto", "login", "list", "refresh", "update", "import-auth", "import-known"}


def main() -> int:
    argv = normalize_argv(sys.argv[1:])
    parser = build_parser()
    args, extra_codex_args = parser.parse_known_args(argv)
    args.extra_codex_args = extra_codex_args

    state_dir = resolve_state_dir(args.state_dir)
    state = load_state(state_dir)

    if args.command in (None, "launch"):
        return cmd_launch(args, state_dir, state)
    if args.command == "auto":
        return cmd_auto(args, state_dir, state)
    if args.command == "login":
        return cmd_login(args, state_dir, state)
    if args.command == "list":
        return cmd_list(args, state_dir, state)
    if args.command == "refresh":
        return cmd_refresh(args, state_dir, state)
    if args.command == "update":
        return cmd_update(args, state_dir, state)
    if args.command == "import-auth":
        return cmd_import_auth(args, state_dir, state)
    if args.command == "import-known":
        return cmd_import_known(args, state_dir, state)
    if args.command == "passthrough":
        return cmd_passthrough(args, state_dir, state)
    parser.print_help()
    return 1


def normalize_argv(argv: list[str]) -> list[str]:
    root: list[str] = []
    idx = 0
    while idx < len(argv):
        token = argv[idx]
        if token in {"-h", "--help"}:
            return [*root, token, *argv[idx + 1 :]]
        if token == "--state-dir":
            if idx + 1 >= len(argv):
                return [*root, token]
            root.extend(argv[idx : idx + 2])
            idx += 2
            continue
        if token.startswith("--state-dir="):
            root.append(token)
            idx += 1
            continue
        break

    remainder = argv[idx:]
    if not remainder:
        return [*root, "launch"]
    if remainder[0] in KNOWN_COMMANDS:
        return [*root, *remainder]
    return [*root, "passthrough", *remainder]


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=os.environ.get("AUTO_CODEX_PROG", DEFAULT_PROGRAM_NAME),
        description="Switch to the Codex account with the highest remaining quota, then launch Codex."
    )
    parser.add_argument(
        "--state-dir",
        help="Override the script state directory.",
    )
    subparsers = parser.add_subparsers(dest="command")

    launch = subparsers.add_parser(
        "launch",
        help="Refresh usage, switch to the best account, then start Codex.",
    )
    launch.add_argument(
        "--no-import-known",
        action="store_true",
        help="Do not auto-import current ~/.codex/auth.json.",
    )
    launch.add_argument(
        "--no-login",
        action="store_true",
        help="Do not start device auth when no usable account exists.",
    )
    launch.add_argument(
        "--dry-run",
        action="store_true",
        help="Show which account would be selected without switching or launching Codex.",
    )
    launch.add_argument(
        "--no-resume",
        action="store_true",
        help="Always start a fresh Codex session instead of resuming the latest one.",
    )
    launch.add_argument(
        "--no-launch",
        action="store_true",
        help="Switch the account but do not start Codex.",
    )

    auto = subparsers.add_parser("auto", help="Refresh usage, pick the best account, and switch.")
    auto.add_argument(
        "--no-import-known",
        action="store_true",
        help="Do not auto-import current ~/.codex/auth.json.",
    )
    auto.add_argument(
        "--no-login",
        action="store_true",
        help="Do not start device auth when no usable account exists.",
    )
    auto.add_argument(
        "--dry-run",
        action="store_true",
        help="Show which account would be selected without switching ~/.codex/auth.json.",
    )

    login = subparsers.add_parser("login", help="Add one account via codex device auth.")
    login.add_argument(
        "--switch",
        action="store_true",
        help="Switch to the new account after login succeeds.",
    )

    subparsers.add_parser("list", help="Refresh usage, then list known accounts.")
    subparsers.add_parser("refresh", help="Refresh usage for all known accounts and print the latest results.")
    update = subparsers.add_parser("update", help="Update scodex from its install source.")
    update.add_argument(
        "--yes",
        action="store_true",
        help="Skip the install confirmation prompt during update.",
    )
    subparsers.add_parser("passthrough", help=argparse.SUPPRESS)

    import_auth = subparsers.add_parser(
        "import-auth",
        help="Import an auth.json file or a directory containing auth.json.",
    )
    import_auth.add_argument("path", help="Path to auth.json or its parent home directory.")

    subparsers.add_parser(
        "import-known",
        help="Import ~/.codex/auth.json. Set AUTO_CODEX_IMPORT_ACCOUNTS_HUB=1 to also import AI Accounts Hub managed homes.",
    )

    return parser


def resolve_state_dir(override: str | None) -> Path:
    if override:
        return Path(override).expanduser().resolve()
    for env_name in ("AUTO_CODEX_HOME", "CODEX_AUTOSWITCH_HOME"):
        env = os.environ.get(env_name)
        if env:
            return Path(env).expanduser().resolve()
    home = Path.home()
    if sys.platform == "darwin":
        root = home / "Library" / "Application Support"
    else:
        xdg_data_home = os.environ.get("XDG_DATA_HOME")
        if xdg_data_home:
            root = Path(xdg_data_home).expanduser().resolve()
        else:
            root = home / ".local" / "share"
    legacy_dir = root / LEGACY_STATE_BASENAME
    if legacy_dir.exists():
        return legacy_dir
    return root / DEFAULT_STATE_BASENAME


def load_state(state_dir: Path) -> dict:
    state_file = state_dir / "state.json"
    if not state_file.exists():
        return {"version": STATE_VERSION, "accounts": [], "usage_cache": {}}
    with state_file.open("r", encoding="utf-8") as fh:
        data = json.load(fh)
    if not isinstance(data, dict):
        raise SystemExit(f"Invalid state file: {state_file}")
    data.setdefault("version", STATE_VERSION)
    data.setdefault("accounts", [])
    data.setdefault("usage_cache", {})
    if normalize_state_account_paths(state_dir, data):
        save_state(state_dir, data)
    return data


def save_state(state_dir: Path, state: dict) -> None:
    state_dir.mkdir(parents=True, exist_ok=True)
    tmp = state_dir / ".state.json.tmp"
    with tmp.open("w", encoding="utf-8") as fh:
        json.dump(state, fh, ensure_ascii=False, indent=2, sort_keys=True)
        fh.write("\n")
    tmp.replace(state_dir / "state.json")


def normalize_state_account_paths(state_dir: Path, state: dict) -> bool:
    changed = False
    accounts_dir = state_dir / "accounts"

    for account in state.get("accounts", []):
        account_id = account.get("id")
        if not account_id:
            continue

        canonical_home = accounts_dir / str(account_id)
        canonical_auth = canonical_home / "auth.json"
        canonical_config = canonical_home / "config.toml"

        if canonical_auth.exists():
            canonical_auth_str = str(canonical_auth)
            if account.get("auth_path") != canonical_auth_str:
                account["auth_path"] = canonical_auth_str
                changed = True

        if canonical_config.exists():
            canonical_config_str = str(canonical_config)
            if account.get("config_path") != canonical_config_str:
                account["config_path"] = canonical_config_str
                changed = True
        elif account.get("config_path"):
            try:
                existing_config = Path(account["config_path"]).expanduser()
            except Exception:
                existing_config = None
            if existing_config is None or not existing_config.exists():
                account["config_path"] = None
                changed = True

    return changed


def now_ts() -> int:
    return int(time.time())


def decode_identity(auth: dict) -> dict:
    tokens = auth.get("tokens") or {}
    id_token = tokens.get("id_token")
    if not id_token:
        raise ValueError("auth.json is missing tokens.id_token")
    payload = id_token.split(".")[1]
    padding = "=" * (-len(payload) % 4)
    claims = json.loads(base64.urlsafe_b64decode(payload + padding).decode("utf-8"))
    email = (claims.get("email") or "").strip().lower()
    if not email:
        raise ValueError("auth.json is missing email in id_token")
    auth_claims = claims.get("https://api.openai.com/auth") or {}
    account_id = (tokens.get("account_id") or "").strip() or None
    plan = normalize_plan(auth_claims.get("chatgpt_plan_type"))
    return {
        "email": email,
        "account_id": account_id,
        "plan": plan,
    }


def normalize_plan(raw: str | None) -> str | None:
    if raw is None:
        return None
    value = str(raw).strip().lower()
    if not value:
        return None
    if value in {"plus", "free", "pro"}:
        return value.capitalize()
    return value[:1].upper() + value[1:]


def atomic_copy(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    tmp = dst.parent / f".{dst.name}.tmp"
    shutil.copy2(src, tmp)
    tmp.replace(dst)


def import_auth_path(state_dir: Path, state: dict, raw_path: Path) -> dict:
    path = raw_path.expanduser().resolve()
    auth_path = path / "auth.json" if path.is_dir() else path
    if not auth_path.exists():
        raise FileNotFoundError(f"auth.json not found: {auth_path}")
    config_path = auth_path.parent / "config.toml"
    with auth_path.open("r", encoding="utf-8") as fh:
        auth = json.load(fh)
    identity = decode_identity(auth)

    existing = find_matching_account(state, identity["email"], identity["account_id"])
    account_id = existing["id"] if existing else str(uuid.uuid4())
    account_home = state_dir / "accounts" / account_id
    account_home.mkdir(parents=True, exist_ok=True)
    stored_auth_path = account_home / "auth.json"
    atomic_copy(auth_path, stored_auth_path)
    stored_config_path = None
    if config_path.exists():
        stored_config_path = account_home / "config.toml"
        atomic_copy(config_path, stored_config_path)

    timestamp = now_ts()
    record = {
        "id": account_id,
        "email": identity["email"],
        "account_id": identity["account_id"],
        "plan": identity["plan"],
        "auth_path": str(stored_auth_path),
        "config_path": str(stored_config_path) if stored_config_path else None,
        "added_at": existing["added_at"] if existing else timestamp,
        "updated_at": timestamp,
    }

    if existing:
        replace_account(state, record)
    else:
        state["accounts"].append(record)
    save_state(state_dir, state)
    return record


def find_matching_account(state: dict, email: str, account_id: str | None) -> dict | None:
    for account in state["accounts"]:
        if account["email"].lower() == email.lower():
            return account
        if account_id and account.get("account_id") == account_id:
            return account
    return None


def replace_account(state: dict, updated: dict) -> None:
    for idx, account in enumerate(state["accounts"]):
        if account["id"] == updated["id"]:
            state["accounts"][idx] = updated
            return


def cmd_import_auth(args: argparse.Namespace, state_dir: Path, state: dict) -> int:
    record = import_auth_path(state_dir, state, Path(args.path))
    print(f"Imported {record['email']} -> {record['id']}")
    return 0


def cmd_import_known(_: argparse.Namespace, state_dir: Path, state: dict) -> int:
    imported = import_known_sources(state_dir, state)
    if not imported:
        print("No importable accounts found.")
        return 1
    for account in imported:
        print(f"Imported {account['email']} -> {account['id']}")
    return 0


def import_known_sources(state_dir: Path, state: dict) -> list[dict]:
    imported: list[dict] = []
    seen: set[str] = set()

    def maybe_import(path: Path) -> None:
        key = str(path)
        if key in seen or not path.exists():
            return
        seen.add(key)
        try:
            imported.append(import_auth_path(state_dir, state, path))
        except Exception:
            return

    maybe_import(Path.home() / ".codex" / "auth.json")

    if os.environ.get("AUTO_CODEX_IMPORT_ACCOUNTS_HUB", "").strip().lower() not in {
        "1",
        "true",
        "yes",
        "on",
    }:
        return dedupe_imported(imported)

    home = Path.home()
    candidate_roots = [
        home / "Library" / "Application Support" / "com.murong.ai-accounts-hub" / "codex" / "managed-codex-homes",
        home / ".local" / "share" / "com.murong.ai-accounts-hub" / "codex" / "managed-codex-homes",
    ]
    for root in candidate_roots:
        if not root.exists():
            continue
        for auth_path in sorted(root.glob("*/auth.json")):
            maybe_import(auth_path)

    return dedupe_imported(imported)


def dedupe_imported(accounts: list[dict]) -> list[dict]:
    result: list[dict] = []
    seen_ids: set[str] = set()
    for account in accounts:
        if account["id"] in seen_ids:
            continue
        seen_ids.add(account["id"])
        result.append(account)
    return result


def resolve_codex_bin() -> str:
    env = os.environ.get("CODEX_BIN")
    if env:
        return env
    for candidate in ("codex", str(Path.home() / ".local" / "bin" / "codex")):
        resolved = shutil.which(candidate) if candidate == "codex" else candidate
        if resolved and Path(resolved).exists():
            return resolved
    raise SystemExit("Unable to find `codex`. Set CODEX_BIN or install Codex CLI first.")


def cmd_login(args: argparse.Namespace, state_dir: Path, state: dict) -> int:
    record = run_device_auth_login(state_dir, state)
    usage = refresh_account_usage(state_dir, state, record)
    print(f"Added {record['email']}")
    if args.switch:
        switch_account(record)
        print_selection(record, usage, prefix="Switched to")
    return 0


def run_device_auth_login(state_dir: Path, state: dict) -> dict:
    codex_bin = resolve_codex_bin()
    temp_root = state_dir / ".tmp"
    temp_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="codex-autoswitch-login-", dir=temp_root) as tmp:
        tmp_home = Path(tmp)
        env = os.environ.copy()
        env["CODEX_HOME"] = str(tmp_home)
        local_ip = detect_local_ip()

        print("Starting `codex login --device-auth`.")
        print("Open the printed URL on any browser-enabled machine and finish the login there.")
        print(f"Headless host LAN IP: {local_ip}")
        print()
        result = subprocess.run([codex_bin, "login", "--device-auth"], env=env)
        if result.returncode != 0:
            raise SystemExit(result.returncode)

        auth_path = tmp_home / "auth.json"
        if not auth_path.exists():
            raise SystemExit("Login finished but no auth.json was produced.")

        return import_auth_path(state_dir, state, tmp_home)


def cmd_launch(args: argparse.Namespace, state_dir: Path, state: dict) -> int:
    account, usage = ensure_best_account(args, state_dir, state, perform_switch=not args.dry_run)
    if account is None:
        print("No usable account found.")
        return 1

    if args.dry_run:
        print_selection(account, usage, prefix="Would select")
        return 0

    print_selection(account, usage, prefix="Switched to")
    if args.no_launch:
        return 0
    return launch_codex(args.extra_codex_args, resume=not args.no_resume)


def cmd_refresh(_: argparse.Namespace, state_dir: Path, state: dict) -> int:
    return refresh_and_print_accounts(state_dir, state, announce_refresh=True)


def cmd_list(_: argparse.Namespace, state_dir: Path, state: dict) -> int:
    return refresh_and_print_accounts(state_dir, state, announce_refresh=False)


def refresh_and_print_accounts(state_dir: Path, state: dict, *, announce_refresh: bool) -> int:
    if not state["accounts"]:
        print("No accounts.")
        return 1
    refresh_all_accounts(state_dir, state)
    save_state(state_dir, state)
    if announce_refresh:
        print(f"Refreshed {len(state['accounts'])} account(s).")
    accounts = state["accounts"]
    active = read_live_identity()
    print(render_account_table(accounts, state["usage_cache"], active))
    print(f"{len(accounts)} row(s) in set.")
    return 0


def render_account_table(accounts: list[dict], usage_cache: dict, active: dict | None) -> str:
    rows = []
    for account in sorted(accounts, key=lambda item: item["email"]):
        usage = usage_cache.get(account["id"], {})
        plan = account.get("plan") or usage.get("plan") or "Unknown"
        rows.append([
            active_account_marker() if identity_matches(account, active) else "",
            account["email"],
            plan,
            format_percent(usage.get("five_hour_remaining_percent")),
            format_percent(usage.get("weekly_remaining_percent")),
            format_reset_on(usage.get("weekly_refresh_at")),
            format_account_status(usage),
        ])
    return render_ascii_table(
        ["Active", "Email", "Plan", "5h", "Weekly", "ResetOn", "Status"],
        rows,
        aligns=["center", "left", "center", "center", "center", "center", "center"],
    )


def format_account_status(usage: dict) -> str:
    if usage.get("needs_relogin"):
        return "RELOGIN"
    if usage.get("last_sync_error"):
        return "ERROR"
    return "OK"


def active_account_marker() -> str:
    marker = "✓"
    encoding = sys.stdout.encoding or ""
    try:
        marker.encode(encoding)
    except (LookupError, UnicodeEncodeError):
        return "Y"
    return marker


def render_ascii_table(
    headers: list[str],
    rows: list[list[str]],
    aligns: list[str] | None = None,
) -> str:
    if aligns is None:
        aligns = ["left"] * len(headers)
    if len(aligns) != len(headers):
        raise ValueError("aligns must match headers length")

    widths = [
        max([len(str(header)), *(len(str(row[idx])) for row in rows)])
        for idx, header in enumerate(headers)
    ]
    border = "+" + "+".join("-" * (width + 2) for width in widths) + "+"

    def align_cell(value: str, width: int, align: str) -> str:
        if align == "left":
            return value.ljust(width)
        if align == "right":
            return value.rjust(width)
        if align == "center":
            return value.center(width)
        raise ValueError(f"unsupported alignment: {align}")

    def render_row(values: list[str]) -> str:
        cells = [
            align_cell(str(value), widths[idx], aligns[idx])
            for idx, value in enumerate(values)
        ]
        return "| " + " | ".join(cells) + " |"

    lines = [border, render_row(headers), border]
    for row in rows:
        lines.append(render_row(row))
        lines.append(border)
    return "\n".join(lines)


def cmd_auto(args: argparse.Namespace, state_dir: Path, state: dict) -> int:
    account, usage = ensure_best_account(args, state_dir, state, perform_switch=not args.dry_run)
    if account is None:
        print("No usable account found.")
        return 1
    if args.dry_run:
        print_selection(account, usage, prefix="Would select")
        return 0
    print_selection(account, usage, prefix="Switched to")
    return 0


def cmd_passthrough(args: argparse.Namespace, state_dir: Path, state: dict) -> int:
    dispatch_args = argparse.Namespace(no_import_known=False, no_login=False)
    account, usage = ensure_best_account(dispatch_args, state_dir, state, perform_switch=True)
    if account is None:
        print("No usable account found.")
        return 1
    print_selection(account, usage, prefix="Switched to")
    return run_codex_passthrough(args.extra_codex_args)


def cmd_update(args: argparse.Namespace, state_dir: Path, state: dict) -> int:
    _ = state
    install_base = resolve_install_base_url()
    install_url = f"{install_base.rstrip('/')}/install.sh"
    temp_root = state_dir / ".tmp"
    temp_root.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="auto-codex-update-", dir=temp_root) as tmp:
        installer_path = Path(tmp) / "install.sh"
        try:
            with urllib.request.urlopen(install_url, timeout=30) as response:
                installer_path.write_bytes(response.read())
        except Exception as exc:
            raise SystemExit(f"Failed to download installer from {install_url}: {exc}") from exc

        installer_path.chmod(0o755)
        env = os.environ.copy()
        env["AUTO_CODEX_RAW_BASE"] = install_base
        if args.yes:
            env["AUTO_CODEX_YES"] = "1"

        print(f"Updating scodex from {install_url}", flush=True)
        return subprocess.run(["bash", str(installer_path)], env=env).returncode


def ensure_best_account(
    args: argparse.Namespace,
    state_dir: Path,
    state: dict,
    *,
    perform_switch: bool,
) -> tuple[dict | None, dict]:
    if not args.no_import_known:
        import_known_sources(state_dir, state)
    if not state["accounts"]:
        if args.no_login:
            return None, {}
        record = run_device_auth_login(state_dir, state)
        usage = refresh_account_usage(state_dir, state, record)
        if perform_switch:
            switch_account(record)
        return record, usage

    refresh_all_accounts(state_dir, state)
    best = choose_best_account(state)
    if best is None:
        if args.no_login:
            return None, {}
        record = run_device_auth_login(state_dir, state)
        usage = refresh_account_usage(state_dir, state, record)
        if perform_switch:
            switch_account(record)
        return record, usage

    usage = state["usage_cache"].get(best["id"], {})
    if perform_switch:
        switch_account(best)
    return best, usage


def refresh_all_accounts(state_dir: Path, state: dict) -> None:
    accounts = list(state["accounts"])
    if not accounts:
        save_state(state_dir, state)
        return

    max_workers = resolve_refresh_workers(len(accounts))
    if max_workers == 1:
        for account in accounts:
            previous = state["usage_cache"].get(account["id"])
            state["usage_cache"][account["id"]] = fetch_usage_for_account(account, previous)
        save_state(state_dir, state)
        return

    previous_usage = {
        account["id"]: state["usage_cache"].get(account["id"])
        for account in accounts
    }
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        refreshed = executor.map(
            lambda account: (
                account["id"],
                fetch_usage_for_account(account, previous_usage.get(account["id"])),
            ),
            accounts,
        )
        for account_id, usage in refreshed:
            state["usage_cache"][account_id] = usage
    save_state(state_dir, state)


def refresh_account_usage(state_dir: Path, state: dict, account: dict) -> dict:
    usage = fetch_usage_for_account(account, state["usage_cache"].get(account["id"]))
    state["usage_cache"][account["id"]] = usage
    save_state(state_dir, state)
    return usage


def resolve_refresh_workers(account_count: int) -> int:
    override = os.environ.get("AUTO_CODEX_REFRESH_WORKERS")
    if override:
        try:
            return max(1, min(int(override), account_count))
        except ValueError:
            pass
    return max(1, min(account_count, 8))


def fetch_usage_for_account(account: dict, previous: dict | None = None) -> dict:
    auth_path = Path(account["auth_path"])
    config_path = Path(account["config_path"]) if account.get("config_path") else None
    with auth_path.open("r", encoding="utf-8") as fh:
        auth = json.load(fh)
    tokens = auth.get("tokens") or {}
    access_token = (tokens.get("access_token") or "").strip()
    account_id = (tokens.get("account_id") or "").strip()
    timestamp = now_ts()

    if not access_token:
        return merge_usage_with_previous(previous, {
            "plan": account.get("plan"),
            "last_synced_at": timestamp,
            "last_sync_error": "auth.json is missing tokens.access_token",
            "needs_relogin": False,
        })

    url = resolve_usage_url(config_path)
    headers = {
        "Authorization": f"Bearer {access_token}",
        "Accept": "application/json",
        "User-Agent": "codex-cli",
    }
    if account_id:
        headers["ChatGPT-Account-Id"] = account_id

    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        if exc.code == 401:
            return merge_usage_with_previous(previous, {
                "plan": account.get("plan"),
                "last_synced_at": timestamp,
                "last_sync_error": "Codex OAuth token expired or invalid. Run `codex login` again.",
                "needs_relogin": True,
            })
        return merge_usage_with_previous(previous, {
            "plan": account.get("plan"),
            "last_synced_at": timestamp,
            "last_sync_error": f"GET {url} failed: {exc.code}",
            "needs_relogin": False,
        })
    except Exception as exc:
        return merge_usage_with_previous(previous, {
            "plan": account.get("plan"),
            "last_synced_at": timestamp,
            "last_sync_error": str(exc),
            "needs_relogin": False,
        })

    normalized = normalize_usage_response(payload)
    normalized["last_synced_at"] = timestamp
    normalized["last_sync_error"] = None
    normalized["needs_relogin"] = False
    return normalized


def merge_usage_with_previous(previous: dict | None, update: dict) -> dict:
    if not previous:
        return update

    merged = dict(previous)
    merged.update(update)
    return merged


def resolve_install_base_url() -> str:
    return os.environ.get("AUTO_CODEX_RAW_BASE", DEFAULT_INSTALL_BASE_URL).strip().rstrip("/")


def resolve_usage_url(config_path: Path | None) -> str:
    base = DEFAULT_USAGE_BASE_URL
    override = os.environ.get("CODEX_USAGE_BASE_URL")
    if override:
        base = override.strip()
    elif config_path and config_path.exists():
        parsed = parse_chatgpt_base_url(config_path.read_text(encoding="utf-8"))
        if parsed:
            base = parsed
    normalized = normalize_chatgpt_base_url(base)
    if "/backend-api" in normalized:
        return f"{normalized}/wham/usage"
    return f"{normalized}/api/codex/usage"


def parse_chatgpt_base_url(contents: str) -> str | None:
    for raw_line in contents.splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line or "=" not in line:
            continue
        key, value = line.split("=", 1)
        if key.strip() != "chatgpt_base_url":
            continue
        parsed = value.strip().strip('"').strip("'").strip()
        if parsed:
            return parsed
    return None


def normalize_chatgpt_base_url(base: str) -> str:
    normalized = base.strip().rstrip("/") or DEFAULT_USAGE_BASE_URL
    if (
        normalized.startswith("https://chatgpt.com")
        or normalized.startswith("https://chat.openai.com")
    ) and "/backend-api" not in normalized:
        normalized += "/backend-api"
    return normalized


def normalize_usage_response(payload: dict) -> dict:
    rate_limit = payload.get("rate_limit") or {}
    five_hour = None
    weekly = None
    for window in [rate_limit.get("primary_window"), rate_limit.get("secondary_window")]:
        if not window:
            continue
        snapshot, role = map_window(window)
        if role == "five_hour":
            if five_hour is None:
                five_hour = snapshot
            elif weekly is None:
                weekly = snapshot
        elif role == "weekly":
            if weekly is None:
                weekly = snapshot
            elif five_hour is None:
                five_hour = snapshot
        else:
            if five_hour is None:
                five_hour = snapshot
            elif weekly is None:
                weekly = snapshot

    credits = payload.get("credits") or {}
    credits_balance = None
    if not credits.get("unlimited"):
        credits_balance = parse_optional_float(credits.get("balance"))

    return {
        "plan": normalize_plan(payload.get("plan_type")),
        "five_hour_remaining_percent": five_hour["remaining_percent"] if five_hour else None,
        "five_hour_refresh_at": five_hour["reset_at"] if five_hour else None,
        "weekly_remaining_percent": weekly["remaining_percent"] if weekly else None,
        "weekly_refresh_at": weekly["reset_at"] if weekly else None,
        "credits_balance": credits_balance,
    }


def parse_optional_float(value) -> float | None:
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str) and value.strip():
        try:
            return float(value.strip())
        except ValueError:
            return None
    return None


def map_window(window: dict) -> tuple[dict, str]:
    used = max(0, min(int(window.get("used_percent", 100)), 100))
    limit_window_seconds = int(window.get("limit_window_seconds", 0))
    role = "unknown"
    if limit_window_seconds == 18_000:
        role = "five_hour"
    elif limit_window_seconds == 604_800:
        role = "weekly"
    return (
        {
            "remaining_percent": max(0, 100 - used),
            "reset_at": str(window.get("reset_at")),
        },
        role,
    )


def choose_best_account(state: dict) -> dict | None:
    candidates: list[tuple[tuple, dict]] = []
    for account in state["accounts"]:
        usage = state["usage_cache"].get(account["id"], {})
        if usage.get("needs_relogin"):
            continue
        score = build_score(account, usage)
        candidates.append((score, account))
    if not candidates:
        return None
    candidates.sort(key=lambda item: item[0], reverse=True)
    return candidates[0][1]


def build_score(account: dict, usage: dict) -> tuple:
    weekly = usage.get("weekly_remaining_percent")
    five_hour = usage.get("five_hour_remaining_percent")
    credits = usage.get("credits_balance")
    freshness = int(usage.get("last_synced_at") or 0)
    updated = int(account.get("updated_at") or 0)

    def quota_score(value) -> tuple[int, int]:
        if value is None:
            return (0, -1)
        return (1, int(value))

    return (
        *quota_score(five_hour),
        *quota_score(weekly),
        -1.0 if credits is None else float(credits),
        freshness,
        updated,
    )


def switch_account(account: dict) -> None:
    src = Path(account["auth_path"])
    dst = Path.home() / ".codex" / "auth.json"
    atomic_copy(src, dst)


def read_live_identity() -> dict | None:
    auth_path = Path.home() / ".codex" / "auth.json"
    if not auth_path.exists():
        return None
    try:
        with auth_path.open("r", encoding="utf-8") as fh:
            auth = json.load(fh)
        return decode_identity(auth)
    except Exception:
        return None


def identity_matches(account: dict, live: dict | None) -> bool:
    if not live:
        return False
    if live["email"].lower() == account["email"].lower():
        return True
    live_account_id = live.get("account_id")
    return bool(live_account_id and live_account_id == account.get("account_id"))


def print_selection(account: dict, usage: dict, prefix: str) -> None:
    weekly = format_percent(usage.get("weekly_remaining_percent"))
    five_hour = format_percent(usage.get("five_hour_remaining_percent"))
    print(f"{prefix} {account['email']} [weekly={weekly}, 5h={five_hour}]", flush=True)


def format_percent(value) -> str:
    if value is None:
        return "N/A"
    return f"{int(value)}%"


def format_reset_on(value) -> str:
    if value is None:
        return "N/A"
    if isinstance(value, (int, float)):
        return datetime.fromtimestamp(float(value)).astimezone().strftime("%m-%d %H:%M")

    text = str(value).strip()
    if not text or text.lower() in {"none", "null", "n/a"}:
        return "N/A"
    if text.isdigit():
        return datetime.fromtimestamp(float(text)).astimezone().strftime("%m-%d %H:%M")

    normalized = text.replace("Z", "+00:00")
    try:
        parsed = datetime.fromisoformat(normalized)
    except ValueError:
        return "N/A"
    if parsed.tzinfo is not None:
        parsed = parsed.astimezone()
    return parsed.strftime("%m-%d %H:%M")


def detect_local_ip() -> str:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect(("8.8.8.8", 80))
        return sock.getsockname()[0]
    except Exception:
        return "127.0.0.1"
    finally:
        sock.close()


def launch_codex(extra_args: list[str], *, resume: bool) -> int:
    codex_bin = resolve_codex_bin()
    fresh_cmd = build_codex_launch_command(codex_bin, extra_args, resume=False)
    if resume and has_resumable_session(Path.cwd()):
        resume_cmd = build_codex_launch_command(codex_bin, extra_args, resume=True)
        print("Resuming latest Codex session for this directory.")
        returncode = subprocess.run(resume_cmd).returncode
        if returncode == 0:
            return 0
        print("Resume did not complete cleanly; falling back to a fresh Codex session.", file=sys.stderr)
    else:
        print("Starting a fresh Codex session.")
    return subprocess.run(fresh_cmd).returncode


def run_codex_passthrough(extra_args: list[str]) -> int:
    codex_bin = resolve_codex_bin()
    return subprocess.run([codex_bin, *extra_args]).returncode


def build_codex_launch_command(codex_bin: str, extra_args: list[str], *, resume: bool) -> list[str]:
    command = [codex_bin]
    if resume:
        command.extend(["resume", "--last"])
    if "--yolo" not in extra_args:
        command.append("--yolo")
    command.extend(extra_args)
    return command


def has_resumable_session(cwd: Path) -> bool:
    sessions_root = Path.home() / ".codex" / "sessions"
    if not sessions_root.exists():
        return False
    target = str(cwd.resolve())
    for session_file in sorted(sessions_root.glob("**/*.jsonl"), reverse=True):
        try:
            with session_file.open("r", encoding="utf-8") as fh:
                first_line = fh.readline().strip()
            if not first_line:
                continue
            record = json.loads(first_line)
        except Exception:
            continue
        if record.get("type") != "session_meta":
            continue
        payload = record.get("payload") or {}
        if payload.get("originator") != "codex-tui":
            continue
        if payload.get("cwd") == target:
            return True
    return False


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("\nInterrupted.", file=sys.stderr)
        raise SystemExit(130)
