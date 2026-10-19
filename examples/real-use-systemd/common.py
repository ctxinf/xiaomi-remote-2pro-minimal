"""Shared plumbing for the real-use systemd deployment: paths, config, secrets.

Imported by both `gatewayctl.py` (the installer CLI) and `pipeline.py` (the
`end` hook). Standard library only — a hook that needs `pip install` before it
can transcribe is a hook that breaks the first time the venv moves.

JSONC keeps user-facing settings small and supports comments without dependencies.
"""

from __future__ import annotations

import json
import os
import secrets as _secrets
import shutil
import subprocess
import sys
import time
import re
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

CONFIG_PATH = Path(os.environ.get("XIAOMI_GATEWAY_CONFIG") or CONFIG_DIR / "config.jsonc")
HOOK_CONFIG_PATH = CONFIG_PATH.with_name("hook.jsonc")
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
        "command": [],
        "transcript": {
            "log": str(STATE_DIR / "transcripts.jsonl"),
            "max_bytes": 1 << 20,
        },
        # Nested rather than one flat wall of `indicator_*`: the popup has
        # three stages, each with the same two or three questions (show it?
        # how? how long?), and a table per stage puts each answer next to the
        # question instead of making you reassemble it from a prefix.
        "indicator": {
            "enabled": True,
            "cli": "my-custom-gnome-indicator-cli",
            "position": "top-left",
            "animate": "2500ms",
            "indeterminate": "spinner",  # spinner|pulse
            # One stage per table; see Indicator.STAGES / STYLES.
            "recording": {"show": True, "style": "live", "ttl_sec": 300},
            "working": {"show": True, "style": "live"},
            # No `style` for the result: it is a terminal card either way, and
            # it never animates. It does own the one real display duration in
            # the sequence — see Indicator.result_timeout.
            "result": {"show": True, "duration_sec": 2.5, "error_duration_sec": 8},
            "look": {
                "bg": "slate-950/78",
                "border": "white/12",
                "text": "slate-300",
                "padding": "tight",
                "radius": "md",
            },
        },
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
        """A table, by dotted path: `section("output.indicator.recording")`.

        Missing tables come back empty rather than raising, so a caller can
        always `.get(key, default)` its way through a config that predates the
        table it is asking about.
        """
        node: Any = self.data
        for part in name.split("."):
            if not isinstance(node, dict):
                return {}
            node = node.get(part, {})
        return node if isinstance(node, dict) else {}

    def get(self, section: str, key: str, default: Any = None) -> Any:
        value = self.section(section).get(key, default)
        return default if value is None else value


# The flat `[output]` keys this file used to read, and where each one moved.
# `[output]` grew one `indicator_*` key per question until it was a wall of
# prefixes you had to parse by eye; it is tables now, one per stage.
LEGACY_OUTPUT_KEYS = {
    "indicator": "[output.indicator] enabled",
    "indicator_cli": "[output.indicator] cli",
    "indicator_position": "[output.indicator] position",
    "indicator_animate": "[output.indicator] animate",
    "indicator_indeterminate": "[output.indicator] indeterminate",
    "indicator_recording_ttl_sec": "[output.indicator.recording] ttl_sec",
    "indicator_stage_recording": "[output.indicator.recording] show",
    "indicator_stage_working": "[output.indicator.working] show",
    "indicator_stage_result": "[output.indicator.result] show",
    "indicator_style_recording": "[output.indicator.recording] style",
    "indicator_style_working": "[output.indicator.working] style",
    "indicator_bg": "[output.indicator.look] bg",
    "indicator_border": "[output.indicator.look] border",
    "indicator_text": "[output.indicator.look] text",
    "indicator_padding": "[output.indicator.look] padding",
    "indicator_radius": "[output.indicator.look] radius",
    "transcript_log": "[output.transcript] log",
    "transcript_log_max_bytes": "[output.transcript] max_bytes",
}


def _warn_legacy_output(output: dict[str, Any]) -> None:
    """Says so when a config still uses the old flat `[output]` keys.

    Nothing reads them any more, and a renamed key that is silently ignored is
    the worst kind: the setting is right there in the file, and the behaviour
    is the default. One line per stale key, with the new place to put it.
    """
    for old in sorted(set(output) & set(LEGACY_OUTPUT_KEYS)):
        # `indicator` and `transcript` are table names now, and a table under
        # that name is the *new* spelling, not the old one.
        if isinstance(output[old], dict):
            continue
        log(f"[config] '{old}' under [output] is no longer read — "
            f"move it to {LEGACY_OUTPUT_KEYS[old]}")


def read_jsonc(path: Path) -> dict[str, Any]:
    # Match strings before comments so URLs and escaped quotes remain intact.
    pattern = r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*[\s\S]*?\*/'
    def strip(match):
        token = match.group()
        return token if token.startswith('"') else ''.join(
            '\n' if c == '\n' else ' ' for c in token)
    try:
        value = json.loads(re.sub(pattern, strip, path.read_text()))
    except (OSError, ValueError) as e:
        raise Fail(f"invalid JSONC configuration {path}: {e}") from e
    if not isinstance(value, dict):
        raise Fail(f"{path} must contain an object")
    return value


def load_config(path: Path = CONFIG_PATH, *, resolve_secrets: bool = True) -> Config:
    if not path.exists():
        raise Fail(
            f"config not found: {path}\n"
            f"  create it with:  ./install-service.sh config init"
        )
    raw = read_jsonc(path)
    if HOOK_CONFIG_PATH.exists():
        hook = read_jsonc(HOOK_CONFIG_PATH)
        unknown = set(hook) - {"asr", "llm", "output", "audio"}
        if unknown:
            raise Fail(f"unknown hook settings: {sorted(unknown)}")
        raw = _merge(raw, hook)
    unknown = set(raw) - set(DEFAULTS)
    if unknown:
        raise Fail(f"unknown settings: {sorted(unknown)}")
    _warn_legacy_output(raw.get("output", {}))
    data = _merge(DEFAULTS, raw)
    missing: list[str] = []
    if resolve_secrets:
        store = SecretStore().load() if SECRETS_PATH.exists() else {}
        data = _resolve_secrets(data, store, missing)
    return Config(data, path, missing)


# ---------------------------------------------------------------------------
# Desktop feedback
# ---------------------------------------------------------------------------


def notify(cfg: Config, title: str, body: str) -> None:
    """Fallback path: a plain desktop notification, used only when the overlay
    is unavailable."""
    if not cfg.get("output", "notify", True):
        return
    try:
        subprocess.run(
            ["notify-send", "--app-name=Xiaomi Remote",
             "--icon=microphone-sensitivity-high", "--expire-time=5000", title, body],
            check=False, capture_output=True, timeout=5,
        )
    except (FileNotFoundError, subprocess.SubprocessError):
        pass  # A desktop-less session is a fine place to run this.


class Indicator:
    """The one popup that follows a recording from key-down to transcript.

    Three states, one popup on screen:

        按下按键   → "正在录音…"          (start hook)
        松开按键   → "识别中…"            (end hook)
        ASR 返回   → "识别文本：xxx"      (end hook)

    A notification cannot do this. It has no way to say "still working" and
    later *become* the answer — it can only stack a second bubble and leave the
    first in the history. The indicator keys every popup on a stable `--id`, so
    each step rewrites the popup already on screen: no flicker, no leftovers,
    nothing in notification history.

    Two properties of the backend make this work, and they are worth stating
    because the design leans on both:

    - `update --id X` is just `Notify` with that id, and `Notify` *creates* the
      popup when the id is unknown. So no step has to know whether an earlier
      step ran. That matters here more than it looks: steps 1 and 2 happen in
      two different hook processes with no shared state, and the `start` hook
      may well have never fired (service restarted mid-recording, indicator
      only just installed). Every call therefore sends the full set of options,
      so whichever call lands first produces a correct popup.
    - The id is a constant, not per-PID. A run killed mid-ASR leaves its popup
      behind; the next recording must replace that stale popup rather than
      stack below it.

    But a constant id also means a *late* call from a finished recording lands
    on the next one's popup, or resurrects a popup that already went away. That
    is not hypothetical: the `start` hook is spawned detached and nothing waits
    for it, so if its one D-Bus round trip stalls — a busy Shell, a cold CLI
    start — it can arrive after the `end` hook has already shown the transcript
    and the popup has expired, at which point "正在录音…" reappears out of
    nowhere seconds after the recording finished. `claim`/`_owns` close that:
    each hook process writes its own token to `TURN_FILE` when it takes over,
    and every call re-reads the file immediately before and after talking to the
    backend. A process that no longer holds the turn does not draw, and if it
    was overtaken *while* its call was in flight it takes its own popup back
    down. Ordering, not locking — the newest generation always wins.

    Each of the three stages is independently switchable — `show` in its own
    `[output.indicator.<stage>]` table, see `STAGES`. They are genuinely separate preferences:
    someone who only wants to know the mic is live keeps `recording` and drops
    the rest; someone who only wants the transcript keeps `result`. Turning a
    stage off does not just skip its call, it *hides* the popup, because a
    skipped step would otherwise leave the previous step's popup — "正在录音…"
    long after the key came up — sitting there until its ttl expires.

    The two in-progress stages also pick how they render, `style` in the same
    table, see `STYLES`:

        minimal  一行字，没有动画，没有转圈；进行中只有末尾的 "…"
        live     转圈 + 慢速淡入淡出、颜色过渡

    `result` has no such choice, because there is nothing left for it to
    choose: it is the end of the sequence, nothing is still running, so there
    is no spinner, and it never animates either — motion on the one line you
    are trying to read is just noise.

    No icon and no title, in either style. Both are fixed strings that say
    nothing a user does not already know — they only cost a line of height and
    push the actual text down — and height is the whole budget for something
    that appears every time you speak. `--icon ''` clears the icon explicitly
    rather than relying on the type default; the title is simply never sent.

    On motion: every step here is **one** call. Nothing in this file loops to
    animate anything. The Shell interpolates between two values on its own — a
    changed `--accent` cross-fades — so driving an animation by re-invoking the
    CLI on a timer would be both wasteful (a process spawn and a D-Bus round
    trip per frame) and worse-looking than what the compositor already does.

    The `live` in-progress steps are a spinner, not a progress bar. A bar has to
    claim a position, and there is no honest one to claim: recording length is
    unknown until the key comes up, and an ASR call is one opaque round trip. A
    bar stuck at "indeterminate" is a bar pretending; a spinner says "working"
    and nothing else. `indeterminate = "pulse"` puts the sliding bar
    back. Its cycle belongs to the extension; `--animate` does not touch it.

    The trailing "…" is the whole in-progress vocabulary: `正在录音…` is
    working, `识别文本：xxx` is done.
    """

    ID = "xiaomi-asr"

    # Whose turn it is to draw. One line, rewritten by each hook process as it
    # takes over; see the class docstring.
    TURN_FILE = STATE_DIR / "indicator.turn"

    # The three stages, in the order they happen. Each is a table of its own:
    # `[output.indicator.recording]` and so on.
    STAGES = (
        "recording",  # 按下按键 → 正在录音…
        "working",    # 松开按键 → 识别中… / 后处理中…
        "result",     # ASR 返回 → 识别文本：xxx
    )

    STYLES = ("minimal", "live")

    # Per-step accent — the one thing that changes colour between steps, and
    # the Shell cross-fades it for free, so the popup reads as one object
    # moving through states rather than three popups sharing a spot on screen.
    # Red while the mic is live is the convention worth borrowing from every
    # recording indicator ever made.
    #
    # Muted on purpose: this thing is up for a few seconds during *every*
    # sentence you speak, so it has to be readable at a glance and ignorable
    # the rest of the time. The accent is the only saturated pixel left, and
    # even it is dialled back — a 4px bar at 80% carries the state fine.
    ACCENT_RECORDING = "rose-400/80"
    ACCENT_WORKING = "sky-400/70"
    ACCENT_DONE = "emerald-400/80"

    def __init__(self, cfg: Config):
        self.cfg = cfg
        ind = cfg.section("output.indicator")
        self.enabled = bool(ind.get("enabled", True))
        self.position = str(ind.get("position", "top-left"))
        self.exe = shutil.which(str(ind.get("cli", "my-custom-gnome-indicator-cli")))
        # One table per stage, each read the same way. The `result` table has no
        # `style`: it has neither a spinner to switch off nor an animation to
        # keep, so the key would be one that does nothing.
        self.stages = {name: cfg.section(f"output.indicator.{name}")
                       for name in self.STAGES}
        # `spinner` unless explicitly asked for the pulse bar. Sent as the long
        # form rather than `--spinner` so that "pulse" is expressible too; on a
        # GNOME older than 49 the extension falls back to the pulse bar by
        # itself, which is why this needs no version check.
        self.indeterminate = ("pulse" if str(
            ind.get("indeterminate", "spinner")).lower() == "pulse"
            else "spinner")
        # Glacial on purpose, and far past the CLI's `slow` (400ms).
        # `--animate` takes a raw duration, and a longer one costs nothing
        # here: the Shell owns every frame, so a 2.5s cross-fade and a 200ms
        # one are the same amount of work for this script — exactly one call.
        # What it buys is a popup that never snaps: it seeps in while you are
        # still talking and drifts between states instead of flicking, which is
        # the difference between something you register and something that
        # interrupts you.
        self.animate = str(ind.get("animate", "2500ms"))
        # Appearance, sent on every call: a step that only passed `--accent`
        # would inherit whatever the *previous* run left, and a popup created
        # by step 2 (because step 1 never fired) has nothing to inherit at all.
        look = cfg.section("output.indicator.look")
        self.look = [
            # Quiet by default. This appears on every sentence you speak, so
            # it has to be legible at a glance and easy to ignore the rest of
            # the time; a loud box would turn into something you resent.
            "--bg", str(look.get("bg", "slate-950/78")),
            "--border", str(look.get("border", "white/12")),
            "--text", str(look.get("text", "slate-300")),
            "--padding", str(look.get("padding", "tight")),
            "--radius", str(look.get("radius", "md")),
            # One line, always. The icon is the other half of the height bill
            # (see the class docstring); `''` is the documented way to drop it.
            "--icon", "",
        ]
        self.turn: str | None = None

    @property
    def active(self) -> bool:
        return self.enabled and self.exe is not None

    def shows(self, stage: str) -> bool:
        """Whether `stage` should put anything on screen at all."""
        return self.active and bool(self.stages.get(stage, {}).get("show", True))

    def style_of(self, stage: str) -> str:
        """`minimal` or `live` for one stage, falling back to `live` on a typo."""
        want = str(self.stages.get(stage, {}).get("style", "live")).lower()
        return want if want in self.STYLES else "live"

    def ttl_of(self, stage: str, default: int) -> int:
        """That stage's dead-man timer, in seconds. See `progress`."""
        try:
            return int(float(self.stages.get(stage, {}).get("ttl_sec", default)))
        except (TypeError, ValueError):
            return default

    def result_timeout(self, kind: str) -> str:
        """How long the result stays on screen, as a `--timeout` value.

        The only genuine *display duration* of the three. Stages 1 and 2 are
        normally overwritten by the next stage within milliseconds of their own
        step ending, so what `ttl_sec` buys them is a ceiling for when that
        never happens; nobody watches a popup for `ttl_sec` seconds. This one
        is different: nothing follows it, so the number here is exactly how
        long you look at it.

        Two numbers because there are two jobs. A result you can already paste
        needs a glance — the text hit the clipboard before this popup existed,
        so it is a receipt, and a receipt that outstays the glance is just
        something in the way. An error you have to read, so it gets longer.

        Seconds in, milliseconds out: `2.5` is a reasonable thing to write for
        a receipt, and the CLI's duration parser is happier with `2500ms` than
        with a fractional `2.5s`.
        """
        bad = kind in ("error", "warning", "warn")
        key = "error_duration_sec" if bad else "duration_sec"
        try:
            seconds = float(self.stages.get("result", {}).get(key, 8 if bad else 2.5))
        except (TypeError, ValueError):
            seconds = 8 if bad else 2.5
        # `0` means "stay until something replaces it" to the backend, and it
        # is a legitimate choice here: the popup then sits there until the next
        # recording overwrites it. Anything negative is a typo, not a choice.
        return f"{max(0, int(seconds * 1000))}ms"

    # -- whose turn it is ---------------------------------------------------

    def claim(self) -> None:
        """Declares this process the current owner of the popup.

        Called once per hook process, as early as possible. Everything after it
        is ordered against other processes by `_owns`: the `end` hook claiming
        the turn is what stops a stalled `start` hook from drawing "正在录音…"
        over — or after — the result it produced.
        """
        if not self.active:
            return
        self.turn = f"{os.getpid()}:{time.time():.6f}"
        try:
            self.TURN_FILE.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.TURN_FILE.with_suffix(".turn.tmp")
            tmp.write_text(self.turn, encoding="utf-8")
            tmp.replace(self.TURN_FILE)  # atomic: no reader sees a half file
        except OSError as e:
            # Losing the ordering is survivable — worst case is the old
            # behaviour — so this must never take the hook down with it.
            log(f"[ui] indicator turn file unwritable: {e}")
            self.turn = None

    def _owns(self) -> bool:
        """Whether this process is still the one allowed to draw."""
        if self.turn is None:
            return True  # never claimed, or the file is unusable: old behaviour
        try:
            return self.TURN_FILE.read_text(encoding="utf-8").strip() == self.turn
        except OSError:
            return True

    def _call(self, args: list[str], *, warn: bool = True, guard: bool = True) -> bool:
        if guard and not self._owns():
            # A newer recording owns the screen. Saying so is worth one line:
            # this is exactly the path that used to produce a resurrected
            # "正在录音…" popup, and silence here would make it unexplainable.
            log(f"[ui] indicator skipped ({args[0]}): superseded by a newer run")
            return False
        try:
            proc = subprocess.run([self.exe, *args], capture_output=True, timeout=5)
        except (OSError, subprocess.SubprocessError) as e:
            log(f"[ui] indicator failed: {e}")
            return False
        if proc.returncode != 0:
            if warn:
                log(f"[ui] indicator exit {proc.returncode}: "
                    f"{proc.stderr.decode(errors='replace').strip()[:200]}")
            return False
        return True

    def _appearance(self, stage: str) -> list[str]:
        """Style args for one stage: everything but the type and the body.

        `result` is pinned to `--animate off` whatever its style says. It is the
        end of the sequence — nothing follows it, nothing is still running — so
        a fade-in would only delay the one line here anybody actually reads.
        """
        animate = self.animate
        if stage == "result" or self.style_of(stage) == "minimal":
            animate = "off"
        return ["--position", self.position, "--animate", animate, *self.look]

    def progress(self, body: str, *, stage: str, ttl_sec: int,
                 accent: str = ACCENT_WORKING) -> bool:
        """Puts up (or rewrites) the in-progress popup for one step.

        A stage switched off in config does not simply return: it takes down
        whatever the previous stage left, so turning off `working` gives you
        "正在录音…" disappearing when the key comes up, not hanging around for
        the rest of its ttl.

        `ttl_sec` is that step's own dead-man timer, and every step needs one;
        it is the *default*, overridable per stage with `ttl_sec` in that
        stage's table — the caller's number is derived from the relevant
        network timeout, which is the right answer for everything but taste.
        In the normal case the next step overwrites this popup long before the
        timer matters; the timer is for the cases where the next step never
        comes — the `end` hook does not fire because the recording was too
        short to produce a file, the service is restarted mid-recording, the
        process is SIGKILLed mid-ASR. A `--timeout 0` popup in any of those
        situations stays on screen until the next recording.

        `--no-fallback` is equally deliberate. Left alone the CLI answers a
        missing backend with notify-send, which for a *progress* step means an
        extra notification per recording that nothing ever updates or clears.
        With no overlay there is no progress to report, so these steps are
        allowed to fail silently; only the terminal state degrades (see
        `announce`), leaving exactly one message either way.

        Never a percentage: there is nothing to report but "still running",
        which the `live` style says with a spinner and the `minimal` style says
        with nothing at all. `body` carries the trailing "…" either way.
        """
        if not self.shows(stage):
            self.hide()
            return False

        if self.style_of(stage) == "minimal":
            # A plain `info`, not a `progress` with the motion switched off: a
            # progress popup still reserves room for its indicator, and the
            # point of this style is that there is nothing to reserve room for.
            head = ["info", body]
        else:
            # `--progress -1`, not the positional form: the CLI's option loop
            # rejects anything matching `-*` before it can be read as a
            # positional, so `progress "body" -1` dies with "unknown option:
            # -1". Passing it as an option value sidesteps the loop entirely.
            head = ["progress", body, "--progress", "-1",
                    "--indeterminate", self.indeterminate]

        shown = self._call([
            *head, "--id", self.ID, "--accent", accent, *self._appearance(stage),
            "--timeout", f"{self.ttl_of(stage, int(ttl_sec))}s",
            "--no-fallback", "-q",
        ])
        # Overtaken while that call was in flight. Whatever the newer run had
        # on screen, this call just overwrote it — same id — so there is
        # nothing left to protect and the popup now showing is ours and stale.
        # Take it down unguarded; the newer run redraws if it still has a step
        # to show, and if it does not, the alternative is "正在录音…" hanging
        # there for its full ttl over a recording that finished long ago.
        if shown and not self._owns():
            log("[ui] indicator: overtaken mid-call, retracting")
            self._call(["hide", self.ID, "-q"], warn=False, guard=False)
        return shown

    def finish(self, kind: str, body: str) -> bool:
        """Turns the popup into the terminal state, which then expires on its own.

        This is what "取消刚才那条、换成结果" compiles down to: one call, same
        id. Dismissing first and posting second would flash the screen empty in
        between. The type change is what ends the motion: `progress` becomes
        `success` and the spinner is gone. The transition itself is not
        animated — see `_appearance`.

        Only `success` takes the green accent; `warning`/`error` keep the type's
        built-in colours, which are already the right ones for bad news.

        How long it stays is `result_timeout`, which is the only real display
        duration in the whole sequence — see the note there.

        Fallback stays *on* here — a result is worth a notification even in a
        session with no extension loaded. Same reason this returns False when
        the `result` stage is switched off *after* taking the in-progress popup
        down: `announce` reads that False as "the overlay said nothing" and
        sends the notification instead, so the one-message rule still holds.
        Switching the result off entirely is `notify = false` next to it.
        """
        if not self.shows("result"):
            self.hide()
            return False
        kind = {"warn": "warning"}.get(kind, kind)
        accent = [] if kind != "success" else ["--accent", self.ACCENT_DONE]
        return self._call([
            kind, body, "--id", self.ID, "--timeout", self.result_timeout(kind),
            *accent, *self._appearance("result"), "-q",
        ])

    def hide(self) -> None:
        """Takes the popup down with nothing to replace it.

        Only for the paths that end without a result to show — a recording
        rejected as too short, say. Without this the "正在录音…" popup would
        linger until its ttl for an utterance that was never processed.
        """
        if self.active:
            # "no such popup" is the expected answer whenever the `start` hook
            # did not run, and is not worth a line in the log.
            self._call(["hide", self.ID, "-q"], warn=False)


def announce(cfg: Config, ind: Indicator, kind: str, title: str, body: str) -> None:
    """Says one terminal thing, through whichever channel exists.

    Never both: the same transcript as an overlay *and* as a notification is
    worse than seeing it once. With the `result` stage switched off the overlay
    declines, and the notification is what is left.
    """
    if not (ind.active and ind.finish(kind, body)):
        notify(cfg, title, body)


# ---------------------------------------------------------------------------


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
