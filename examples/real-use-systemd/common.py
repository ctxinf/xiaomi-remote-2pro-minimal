"""Shared plumbing for the real-use systemd deployment: paths, config, secrets.

Imported by both `gatewayctl.py` (the installer CLI) and `pipeline.py` (the
`end` hook). Standard library only — a hook that needs `pip install` before it
can transcribe is a hook that breaks the first time the venv moves.

Why TOML and not YAML: `tomllib` ships with Python 3.11+, so the config parses
with zero dependencies, and unlike JSON it takes comments — which matters for a
single file that is meant to be hand-edited. YAML would mean a PyYAML dep on
every machine the service is installed on, for no gain here.
"""

from __future__ import annotations

import json
import os
import secrets as _secrets
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any

APP = "xiaomi-remote-gateway"

# ---------------------------------------------------------------------------
# Paths (XDG)
# ---------------------------------------------------------------------------


def _xdg(var: str, fallback: str) -> Path:
    return Path(os.environ.get(var) or Path.home() / fallback).expanduser()


CONFIG_DIR = _xdg("XDG_CONFIG_HOME", ".config") / APP
STATE_DIR = _xdg("XDG_STATE_HOME", ".local/state") / APP
DATA_DIR = _xdg("XDG_DATA_HOME", ".local/share") / APP

CONFIG_PATH = Path(os.environ.get("XIAOMI_GATEWAY_CONFIG") or CONFIG_DIR / "config.toml")
SECRETS_PATH = CONFIG_DIR / "secrets.enc"
SECRETS_KEY_PATH = CONFIG_DIR / "secrets.key"

# Where `gatewayctl install` materialises the runnable copy, so the service no
# longer depends on the git checkout staying where it was.
INSTALL_BIN_DIR = DATA_DIR / "bin"
INSTALL_HOOK_DIR = DATA_DIR / "hook"

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent


def log(msg: str) -> None:
    """Logs to stderr: the gateway relays a hook's stdout at INFO and stderr at
    WARN, and keeping diagnostics off stdout makes a failure easy to spot."""
    print(msg, file=sys.stderr, flush=True)


class Fail(Exception):
    """An error with a message already fit for a user to read."""


# ---------------------------------------------------------------------------
# Secret store
# ---------------------------------------------------------------------------


class SecretStore:
    """Secrets at rest, encrypted with a local key file.

    The whole store is one JSON object encrypted as a unit with
    `openssl enc -aes-256-cbc -pbkdf2`, keyed by 32 random bytes in
    `secrets.key` (mode 0600). Both files live in the config dir.

    What this does and does not buy you, stated plainly: the credentials are no
    longer sitting in cleartext in a file that gets caught by a backup, a
    `grep -r`, a screen share, or an accidental `git add`. It is **not** a
    defence against someone who can already read files as your user — they can
    read `secrets.key` too. Real sealing needs a TPM (`systemd-creds encrypt
    --with-key=host`, see the README), which is unavailable to user units on
    current systemd.
    """

    def __init__(self, path: Path = SECRETS_PATH, key_path: Path = SECRETS_KEY_PATH):
        self.path = path
        self.key_path = key_path

    # -- key ---------------------------------------------------------------

    def _openssl(self) -> str:
        exe = shutil.which("openssl")
        if not exe:
            raise Fail("openssl not found; install it (dnf install openssl) to store secrets")
        return exe

    def _ensure_key(self) -> str:
        if not self.key_path.exists():
            self.key_path.parent.mkdir(parents=True, exist_ok=True)
            # Written 0600 from the start: creating it world-readable and
            # chmod'ing after would leave a window where the key is exposed.
            fd = os.open(self.key_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w") as fh:
                fh.write(_secrets.token_hex(32))
            log(f"[secret] generated new key {self.key_path}")
        mode = self.key_path.stat().st_mode & 0o777
        if mode & 0o077:
            # A key another user can read is not a key.
            self.key_path.chmod(0o600)
            log(f"[secret] tightened permissions on {self.key_path} ({mode:o} -> 600)")
        return self.key_path.read_text().strip()

    # -- crypto ------------------------------------------------------------

    def _run_openssl(self, args: list[str], data: bytes, passphrase: str) -> bytes:
        # The passphrase goes over a dedicated pipe fd, never argv (which is
        # world-readable in /proc) and never an env var.
        r, w = os.pipe()
        try:
            os.write(w, passphrase.encode())
            os.close(w)
            w = -1
            proc = subprocess.run(
                [self._openssl(), "enc", *args, "-pbkdf2", "-iter", "600000",
                 "-pass", f"fd:{r}"],
                input=data,
                capture_output=True,
                pass_fds=(r,),
            )
        finally:
            if w != -1:
                os.close(w)
            os.close(r)
        if proc.returncode != 0:
            raise Fail(f"openssl failed: {proc.stderr.decode(errors='replace').strip()}")
        return proc.stdout

    def load(self) -> dict[str, str]:
        if not self.path.exists():
            return {}
        raw = self._run_openssl(["-d", "-aes-256-cbc", "-base64", "-A"],
                                self.path.read_bytes(), self._ensure_key())
        try:
            return json.loads(raw or b"{}")
        except json.JSONDecodeError as e:
            raise Fail(f"{self.path} did not decrypt to valid JSON ({e}); "
                       f"was secrets.key replaced?") from e

    def save(self, values: dict[str, str]) -> None:
        blob = json.dumps(values, ensure_ascii=False, indent=2).encode()
        out = self._run_openssl(["-e", "-aes-256-cbc", "-base64", "-A"],
                                blob, self._ensure_key())
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "wb") as fh:
            fh.write(out)
        # Atomic swap, so a crash mid-write cannot leave a truncated store.
        tmp.replace(self.path)

    def set(self, name: str, value: str) -> None:
        values = self.load()
        values[name] = value
        self.save(values)

    def remove(self, name: str) -> bool:
        values = self.load()
        if name not in values:
            return False
        del values[name]
        self.save(values)
        return True

    def names(self) -> list[str]:
        return sorted(self.load())


# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

DEFAULTS: dict[str, Any] = {
    "service": {
        "name": APP,
        "description": "Xiaomi Remote Pro 2 voice gateway (voice -> ASR -> LLM)",
        "restart_sec": 5,
    },
    "gateway": {
        "binary": "auto",
        "record_dir": "",
        "gain_db": 0.0,
        "keep_hours": 24,
        "keep_max": 100,
        "voice_key": "auto",
        "device_name": "",
        "verbose": False,
        "extra_args": [],
    },
    "asr": {
        "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "model": "qwen3-asr-flash",
        "api_key": "",
        "language": "zh",
        "timeout_sec": 60,
        "max_audio_bytes": 8 * 1024 * 1024,
    },
    "llm": {
        "enabled": True,
        "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "model": "qwen-flash",
        "api_key": "",
        "system_prompt": "You are a transcript cleaner.",
        "temperature": 0.2,
        "timeout_sec": 60,
        "max_input_chars": 4000,
    },
    "output": {
        "use": "llm",
        "clipboard": True,
        "notify": True,
        "transcript_log": str(STATE_DIR / "transcripts.jsonl"),
        "transcript_log_max_bytes": 1 << 20,
        "command": [],
    },
    "audio": {
        "delete_after": True,
        "min_duration_sec": 0.3,
    },
}


def _merge(base: dict[str, Any], over: dict[str, Any]) -> dict[str, Any]:
    out = dict(base)
    for key, value in over.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], value)
        else:
            out[key] = value
    return out


def _resolve_secrets(value: Any, store: dict[str, str], missing: list[str]) -> Any:
    """Expands `${secret:name}` placeholders anywhere in the config tree.

    Unknown names become the empty string and are collected in `missing` rather
    than raising: the installer wants to report *all* the secrets you still owe
    it, not just the first one.
    """
    if isinstance(value, str):
        if value.startswith("${secret:") and value.endswith("}"):
            name = value[len("${secret:"): -1]
            if name not in store:
                missing.append(name)
                return ""
            return store[name]
        return value
    if isinstance(value, dict):
        return {k: _resolve_secrets(v, store, missing) for k, v in value.items()}
    if isinstance(value, list):
        return [_resolve_secrets(v, store, missing) for v in value]
    return value


class Config:
    def __init__(self, data: dict[str, Any], path: Path, missing_secrets: list[str]):
        self.data = data
        self.path = path
        self.missing_secrets = missing_secrets

    def section(self, name: str) -> dict[str, Any]:
        return self.data.get(name, {})

    def get(self, section: str, key: str, default: Any = None) -> Any:
        return self.data.get(section, {}).get(key, default)


def load_config(path: Path = CONFIG_PATH, *, resolve_secrets: bool = True) -> Config:
    if not path.exists():
        raise Fail(
            f"config not found: {path}\n"
            f"  create it with:  ./install-service.sh config init"
        )
    with path.open("rb") as fh:
        raw = tomllib.load(fh)
    data = _merge(DEFAULTS, raw)
    missing: list[str] = []
    if resolve_secrets:
        store = SecretStore().load() if SECRETS_PATH.exists() else {}
        data = _resolve_secrets(data, store, missing)
    return Config(data, path, missing)


def which_binary(configured: str) -> Path:
    """Resolves `gateway.binary`: an explicit path, or `auto` to hunt for the
    release build next to this checkout, then anything on PATH."""
    if configured and configured != "auto":
        candidate = Path(configured).expanduser()
        if not candidate.exists():
            raise Fail(f"gateway.binary points at a missing file: {candidate}")
        return candidate.resolve()

    for candidate in (
        INSTALL_BIN_DIR / "xiaomi-remote-pro2-gateway",
        REPO_ROOT / "target" / "release" / "xiaomi-remote-pro2-gateway",
    ):
        if candidate.exists():
            return candidate.resolve()
    found = shutil.which("xiaomi-remote-pro2-gateway")
    if found:
        return Path(found).resolve()
    raise Fail(
        "gateway binary not found; build it first:\n"
        f"  cargo build --release --manifest-path {REPO_ROOT / 'Cargo.toml'}"
    )
