import argparse
import base64
import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("codex-autoswitch.py")
SPEC = importlib.util.spec_from_file_location("codex_autoswitch", SCRIPT_PATH)
autoswitch = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = autoswitch
SPEC.loader.exec_module(autoswitch)


def fake_jwt(payload: dict) -> str:
    def encode(part: dict) -> str:
        raw = json.dumps(part, separators=(",", ":")).encode("utf-8")
        return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")

    return f"{encode({'alg': 'none'})}.{encode(payload)}.sig"


def write_auth(path: Path, email: str, account_id: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    token = fake_jwt({"email": email, "exp": 4_000_000_000})
    path.write_text(
        json.dumps(
            {
                "auth_mode": "chatgpt",
                "tokens": {
                    "access_token": token,
                    "refresh_token": f"refresh-{email}",
                    "id_token": token,
                    "account_id": account_id,
                },
            }
        ),
        encoding="utf-8",
    )


class CodexAutoswitchTest(unittest.TestCase):
    def test_render_account_table_centers_non_email_columns(self) -> None:
        accounts = [
            {"id": "a", "email": "a@example.com", "updated_at": 1, "plan": "Plus"},
            {"id": "b", "email": "longer@example.com", "updated_at": 1, "plan": "Pro"},
        ]
        usage_cache = {
            "a": {
                "weekly_remaining_percent": 80,
                "five_hour_remaining_percent": 90,
                "weekly_refresh_at": "2026-04-20T00:00:00Z",
            },
            "b": {
                "weekly_remaining_percent": 3,
                "five_hour_remaining_percent": 7,
                "weekly_refresh_at": "2026-04-21T00:00:00Z",
                "needs_relogin": True,
            },
        }

        rendered = autoswitch.render_account_table(
            accounts,
            usage_cache,
            {"email": "a@example.com"},
        )

        self.assertIn(
            "|   \u2713    | a@example.com      | Plus | 90% |  80%   | 04-20 08:00 |    OK   |",
            rendered,
        )
        self.assertIn(
            "|        | longer@example.com | Pro  |  7% |   3%   | 04-21 08:00 | RELOGIN |",
            rendered,
        )

    def test_choose_best_account_prefers_five_hour_quota(self) -> None:
        state = {
            "accounts": [
                {"id": "weekly-heavy", "email": "weekly@example.com", "updated_at": 1},
                {"id": "five-heavy", "email": "five@example.com", "updated_at": 1},
            ],
            "usage_cache": {
                "weekly-heavy": {
                    "weekly_remaining_percent": 95,
                    "five_hour_remaining_percent": 5,
                    "credits_balance": 0,
                    "last_synced_at": 10,
                },
                "five-heavy": {
                    "weekly_remaining_percent": 60,
                    "five_hour_remaining_percent": 80,
                    "credits_balance": 0,
                    "last_synced_at": 10,
                },
            },
        }

        best = autoswitch.choose_best_account(state)

        self.assertIsNotNone(best)
        self.assertEqual(best["id"], "five-heavy")

    def test_cmd_use_switches_to_exact_email_case_insensitively(self) -> None:
        state = {
            "version": 1,
            "accounts": [
                {"id": "a", "email": "lauzhihao@qq.com", "updated_at": 1},
            ],
            "usage_cache": {
                "a": {
                    "weekly_remaining_percent": 60,
                    "five_hour_remaining_percent": 80,
                }
            },
        }
        original_import_known = autoswitch.import_known_sources
        original_switch = autoswitch.switch_account
        switched: list[dict] = []
        output = io.StringIO()

        autoswitch.import_known_sources = lambda state_dir, current_state: []
        autoswitch.switch_account = lambda account: switched.append(account)
        try:
            with contextlib.redirect_stdout(output):
                rc = autoswitch.cmd_use(
                    argparse.Namespace(email="LauZhiHao@qq.com"),
                    Path("/tmp/state"),
                    state,
                )
        finally:
            autoswitch.import_known_sources = original_import_known
            autoswitch.switch_account = original_switch

        self.assertEqual(rc, 0)
        self.assertEqual(len(switched), 1)
        self.assertEqual(switched[0]["email"], "lauzhihao@qq.com")
        self.assertIn("Switched to lauzhihao@qq.com [weekly=60%, 5h=80%]", output.getvalue())

    def test_cmd_use_returns_error_for_unknown_email(self) -> None:
        state = {"version": 1, "accounts": [], "usage_cache": {}}
        original_import_known = autoswitch.import_known_sources
        original_switch = autoswitch.switch_account
        output = io.StringIO()

        autoswitch.import_known_sources = lambda state_dir, current_state: []
        autoswitch.switch_account = lambda account: (_ for _ in ()).throw(AssertionError("should not switch"))
        try:
            with contextlib.redirect_stdout(output):
                rc = autoswitch.cmd_use(
                    argparse.Namespace(email="missing@example.com"),
                    Path("/tmp/state"),
                    state,
                )
        finally:
            autoswitch.import_known_sources = original_import_known
            autoswitch.switch_account = original_switch

        self.assertEqual(rc, 1)
        self.assertIn("Unknown account: missing@example.com", output.getvalue())

    def test_import_known_sources_skips_ai_accounts_hub_homes_by_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            tmp_home = Path(tmp_dir)
            managed_home = (
                tmp_home
                / "Library"
                / "Application Support"
                / "com.murong.ai-accounts-hub"
                / "codex"
                / "managed-codex-homes"
                / "acct-1"
            )
            write_auth(managed_home / "auth.json", "hub@example.com", "acct-hub")

            previous_home = os.environ.get("HOME")
            os.environ["HOME"] = str(tmp_home)
            state_dir = tmp_home / ".local" / "share" / "auto-codex"
            state = {"version": 1, "accounts": [], "usage_cache": {}}
            try:
                imported = autoswitch.import_known_sources(state_dir, state)
            finally:
                if previous_home is None:
                    os.environ.pop("HOME", None)
                else:
                    os.environ["HOME"] = previous_home

            self.assertEqual(imported, [])

    def test_import_known_sources_includes_ai_accounts_hub_homes_when_enabled(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            tmp_home = Path(tmp_dir)
            managed_home = (
                tmp_home
                / "Library"
                / "Application Support"
                / "com.murong.ai-accounts-hub"
                / "codex"
                / "managed-codex-homes"
                / "acct-1"
            )
            write_auth(managed_home / "auth.json", "hub@example.com", "acct-hub")

            previous_home = os.environ.get("HOME")
            previous_import_flag = os.environ.get("AUTO_CODEX_IMPORT_ACCOUNTS_HUB")
            os.environ["HOME"] = str(tmp_home)
            os.environ["AUTO_CODEX_IMPORT_ACCOUNTS_HUB"] = "1"
            state_dir = tmp_home / ".local" / "share" / "auto-codex"
            state = {"version": 1, "accounts": [], "usage_cache": {}}
            try:
                imported = autoswitch.import_known_sources(state_dir, state)
            finally:
                if previous_home is None:
                    os.environ.pop("HOME", None)
                else:
                    os.environ["HOME"] = previous_home
                if previous_import_flag is None:
                    os.environ.pop("AUTO_CODEX_IMPORT_ACCOUNTS_HUB", None)
                else:
                    os.environ["AUTO_CODEX_IMPORT_ACCOUNTS_HUB"] = previous_import_flag

            self.assertEqual(len(imported), 1)
            self.assertEqual(imported[0]["email"], "hub@example.com")

    def test_refresh_all_accounts_fetches_in_parallel(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            state_dir = Path(tmp_dir)
            state = {
                "version": 1,
                "accounts": [
                    {"id": "a", "email": "a@example.com", "updated_at": 1},
                    {"id": "b", "email": "b@example.com", "updated_at": 1},
                ],
                "usage_cache": {},
            }
            lock = threading.Lock()
            both_started = threading.Event()
            started = 0
            in_flight = 0
            max_in_flight = 0
            original_fetch = autoswitch.fetch_usage_for_account

            def fake_fetch(account: dict, previous=None):
                nonlocal started, in_flight, max_in_flight
                with lock:
                    started += 1
                    in_flight += 1
                    max_in_flight = max(max_in_flight, in_flight)
                    if started >= 2:
                        both_started.set()
                both_started.wait(1.0)
                with lock:
                    in_flight -= 1
                return {
                    "plan": "Plus",
                    "weekly_remaining_percent": 80,
                    "five_hour_remaining_percent": 90 if account["id"] == "a" else 70,
                    "credits_balance": 0,
                    "last_synced_at": 1,
                    "last_sync_error": None,
                    "needs_relogin": False,
                }

            autoswitch.fetch_usage_for_account = fake_fetch
            try:
                autoswitch.refresh_all_accounts(state_dir, state)
            finally:
                autoswitch.fetch_usage_for_account = original_fetch

            self.assertGreaterEqual(max_in_flight, 2)
            self.assertEqual(state["usage_cache"]["a"]["five_hour_remaining_percent"], 90)
            self.assertEqual(state["usage_cache"]["b"]["five_hour_remaining_percent"], 70)

    def test_cmd_list_refreshes_before_printing(self) -> None:
        state = {
            "version": 1,
            "accounts": [
                {"id": "a", "email": "a@example.com", "updated_at": 1, "plan": "Plus"},
            ],
            "usage_cache": {
                "a": {
                    "weekly_remaining_percent": 10,
                    "five_hour_remaining_percent": 20,
                }
            },
        }
        calls: list[str] = []
        original_refresh = autoswitch.refresh_all_accounts
        original_save = autoswitch.save_state
        original_identity = autoswitch.read_live_identity

        def fake_refresh(state_dir: Path, current_state: dict) -> None:
            _ = state_dir
            calls.append("refresh")
            current_state["usage_cache"]["a"] = {
                "weekly_remaining_percent": 80,
                "five_hour_remaining_percent": 90,
            }

        def fake_save(state_dir: Path, current_state: dict) -> None:
            _ = state_dir
            _ = current_state
            calls.append("save")

        autoswitch.refresh_all_accounts = fake_refresh
        autoswitch.save_state = fake_save
        autoswitch.read_live_identity = lambda: None
        output = io.StringIO()
        try:
            with contextlib.redirect_stdout(output):
                rc = autoswitch.cmd_list(argparse.Namespace(), Path("/tmp/state"), state)
        finally:
            autoswitch.refresh_all_accounts = original_refresh
            autoswitch.save_state = original_save
            autoswitch.read_live_identity = original_identity

        self.assertEqual(rc, 0)
        self.assertEqual(calls, ["refresh", "save"])
        self.assertIn(
            "|        | a@example.com | Plus | 90% |  80%   |   N/A   |   OK   |",
            output.getvalue(),
        )

    def test_cmd_refresh_prints_latest_usage_after_refresh(self) -> None:
        state = {
            "version": 1,
            "accounts": [
                {"id": "a", "email": "a@example.com", "updated_at": 1, "plan": "Plus"},
            ],
            "usage_cache": {},
        }
        original_refresh = autoswitch.refresh_all_accounts
        original_save = autoswitch.save_state
        original_identity = autoswitch.read_live_identity

        def fake_refresh(state_dir: Path, current_state: dict) -> None:
            _ = state_dir
            current_state["usage_cache"]["a"] = {
                "weekly_remaining_percent": 70,
                "five_hour_remaining_percent": 85,
            }

        autoswitch.refresh_all_accounts = fake_refresh
        autoswitch.save_state = lambda state_dir, current_state: None
        autoswitch.read_live_identity = lambda: None
        output = io.StringIO()
        try:
            with contextlib.redirect_stdout(output):
                rc = autoswitch.cmd_refresh(argparse.Namespace(), Path("/tmp/state"), state)
        finally:
            autoswitch.refresh_all_accounts = original_refresh
            autoswitch.save_state = original_save
            autoswitch.read_live_identity = original_identity

        self.assertEqual(rc, 0)
        rendered = output.getvalue()
        self.assertIn("Refreshed 1 account(s).", rendered)
        self.assertIn(
            "|        | a@example.com | Plus | 85% |  70%   |   N/A   |   OK   |",
            rendered,
        )


if __name__ == "__main__":
    unittest.main()
