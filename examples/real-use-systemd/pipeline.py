#!/usr/bin/env python3
"""`end` hook: recorded WAV -> Aliyun ASR -> OpenAI-compatible LLM -> output.

The gateway runs this with the recording's path in `XIAOMI_AUDIO_FILE` (and the
same payload as JSON on stdin), with the working directory set to this script's
own directory. Everything else comes from `config.toml`; credentials come from
the encrypted secret store (see `common.SecretStore`).

Both hops speak the OpenAI chat-completions shape, which is what Aliyun's
DashScope compatible-mode endpoint exposes:

  1. ASR  — the WAV goes up base64-encoded as an `input_audio` content part to
            an ASR model (`qwen3-asr-flash`), which answers with the transcript.
  2. LLM  — the transcript goes to a text model for cleanup/instruction, using
            the system prompt from the config.

Step 2 is skipped when `[llm] enabled = false`, or when ASR came back empty.
A failure in step 2 is *not* fatal: you still get the raw transcript, because a
usable transcript beats a lost one.
"""

from __future__ import annotations

import base64
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import Config, Fail, load_config, log  # noqa: E402


# ---------------------------------------------------------------------------
# Desktop output
# ---------------------------------------------------------------------------


def notify(cfg: Config, title: str, body: str) -> None:
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


def to_clipboard(text: str) -> bool:
    """Copies via wl-copy (Wayland) or xclip (X11).

    On why this stops at the clipboard instead of typing the text for you, see
    the long note in ../hooks/2.typed/asr_clipboard.sh — short version: Wayland
    deliberately has no SendInput() equivalent, and every workaround fights
    global state (input methods, held modifiers, terminal keybindings).
    """
    for cmd in (["wl-copy"], ["xclip", "-selection", "clipboard"]):
        try:
            # wl-copy daemonizes to serve the selection and holds the pipe open,
            # so it is fed and left alone rather than waited on to completion.
            proc = subprocess.Popen(cmd, stdin=subprocess.PIPE,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except FileNotFoundError:
            continue
        proc.stdin.write(text.encode())
        proc.stdin.close()
        time.sleep(0.2)
        return True
    log("[out] no clipboard tool found (install wl-clipboard or xclip)")
    return False


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------


def post_json(url: str, api_key: str, body: dict[str, Any], timeout: float) -> dict[str, Any]:
    req = urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")[:400]
        # The error body can echo the key back, so surface only the message.
        try:
            detail = json.loads(detail).get("error", {}).get("message", detail)
        except (json.JSONDecodeError, AttributeError):
            pass
        raise Fail(f"HTTP {e.code} from {url}: {detail}") from e
    except urllib.error.URLError as e:
        raise Fail(f"cannot reach {url}: {e.reason}") from e


def extract_text(resp: dict[str, Any]) -> str:
    try:
        content = resp["choices"][0]["message"]["content"]
    except (KeyError, IndexError, TypeError) as e:
        raise Fail(f"unexpected response shape: {json.dumps(resp)[:300]}") from e
    if isinstance(content, list):
        # Some ASR responses return content as a list of typed parts.
        content = "".join(p.get("text", "") for p in content if isinstance(p, dict))
    return (content or "").strip()


# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------


def transcribe(cfg: Config, audio: Path) -> str:
    asr = cfg.section("asr")
    api_key = asr.get("api_key") or ""
    if not api_key:
        raise Fail("no ASR key; add one with:  ./install-service.sh secret add "
                   "dashscope_api_key sk-xxxx")

    size = audio.stat().st_size
    limit = int(asr.get("max_audio_bytes", 8 * 1024 * 1024))
    if size > limit:
        # base64 inflates by ~4/3 on top of this, and the API rejects oversized bodies.
        raise Fail(f"audio too large: {size} bytes > {limit}")

    b64 = base64.b64encode(audio.read_bytes()).decode()
    options: dict[str, Any] = {"enable_itn": False}
    # The API wants a bare code ("zh"); the BCP-47 form ("zh-CN") is rejected.
    language = str(asr.get("language") or "").split("-")[0]
    if language:
        options["language"] = language

    log(f"[asr] {audio.name} {size}B model={asr.get('model')}"
        + (f" lang={language}" if language else ""))
    resp = post_json(
        f"{str(asr['base_url']).rstrip('/')}/chat/completions",
        api_key,
        {
            "model": asr.get("model"),
            "stream": False,
            "asr_options": options,
            "messages": [{"role": "user", "content": [
                {"type": "input_audio",
                 "input_audio": {"data": f"data:audio/wav;base64,{b64}"}},
            ]}],
        },
        float(asr.get("timeout_sec", 60)),
    )
    return extract_text(resp)


def postprocess(cfg: Config, transcript: str) -> str:
    llm = cfg.section("llm")
    api_key = llm.get("api_key") or ""
    if not api_key:
        raise Fail("no LLM key; add one with:  ./install-service.sh secret add "
                   "llm_api_key sk-xxxx  (or set [llm] enabled = false)")

    text = transcript[: int(llm.get("max_input_chars", 4000))]
    log(f"[llm] model={llm.get('model')} chars={len(text)}")
    resp = post_json(
        f"{str(llm['base_url']).rstrip('/')}/chat/completions",
        api_key,
        {
            "model": llm.get("model"),
            "temperature": float(llm.get("temperature", 0.2)),
            "stream": False,
            "messages": [
                {"role": "system", "content": llm.get("system_prompt", "")},
                {"role": "user", "content": text},
            ],
        },
        float(llm.get("timeout_sec", 60)),
    )
    return extract_text(resp)


def rotate(path: Path, max_bytes: int) -> None:
    """Keeps the transcript log bounded: one rename at the size cap, one old
    generation kept.

    Everything else in this deployment is already bounded — the gateway reaps
    recordings by age and count, and journald vacuums its own logs — so an
    append-only JSONL file was the single thing here that grew forever. Two
    generations is the right amount of machinery for it; anything more is what
    logrotate is for.
    """
    if max_bytes <= 0:
        return
    try:
        if path.exists() and path.stat().st_size >= max_bytes:
            path.replace(path.with_suffix(path.suffix + ".1"))
            log(f"[log] rotated {path.name} at {max_bytes} bytes")
    except OSError as e:
        log(f"[log] could not rotate {path}: {e}")


def deliver(cfg: Config, transcript: str, final: str) -> None:
    out = cfg.section("output")
    text = final if out.get("use", "llm") == "llm" and final else transcript

    if out.get("clipboard", True) and to_clipboard(text):
        log(f"[out] copied {len(text)} chars to clipboard — press Ctrl+V")
        notify(cfg, "已复制到剪贴板，按 Ctrl+V 粘贴", text)
    else:
        notify(cfg, "识别完成", text)

    log_path = out.get("transcript_log")
    if log_path:
        path = Path(log_path).expanduser()
        path.parent.mkdir(parents=True, exist_ok=True)
        rotate(path, int(out.get("transcript_log_max_bytes", 1 << 20)))
        record = {
            "time": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "duration_sec": float(os.environ.get("XIAOMI_DURATION_SEC", 0) or 0),
            "transcript": transcript,
            "final": final,
        }
        with path.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(record, ensure_ascii=False) + "\n")

    command = out.get("command") or []
    if command:
        # The text arrives on stdin, not argv: it is arbitrary user speech and
        # has no business being parsed as shell arguments.
        log(f"[out] running {command[0]}")
        try:
            subprocess.run(command, input=text.encode(), check=False,
                           capture_output=True, timeout=30)
        except (FileNotFoundError, subprocess.SubprocessError) as e:
            log(f"[out] command failed: {e}")


# ---------------------------------------------------------------------------


def run(audio: Path, cfg: Config) -> int:
    if cfg.missing_secrets:
        raise Fail("missing secrets: " + ", ".join(sorted(set(cfg.missing_secrets)))
                   + "\n  add them with:  ./install-service.sh secret add <name> <value>")

    duration = float(os.environ.get("XIAOMI_DURATION_SEC", 0) or 0)
    min_duration = float(cfg.get("audio", "min_duration_sec", 0.0))
    if duration and duration < min_duration:
        log(f"[skip] {duration:.2f}s shorter than min_duration_sec={min_duration}")
        return 0

    transcript = transcribe(cfg, audio)
    if not transcript:
        log("[asr] empty transcript")
        notify(cfg, "识别为空", "没有识别到文字")
        return 0
    log(f"[asr] {transcript}")

    final = ""
    if cfg.get("llm", "enabled", True):
        try:
            final = postprocess(cfg, transcript)
            log(f"[llm] {final}")
        except Fail as e:
            # Degrade to the raw transcript rather than dropping the utterance.
            log(f"[llm] skipped: {e}")
            notify(cfg, "后处理失败，使用原始转写", str(e))

    deliver(cfg, transcript, final)
    return 0


def cleanup(audio: Path, cfg: Config | None) -> None:
    """Drops the recording once it has been consumed.

    The `end` hook owns the file — the gateway deliberately never deletes it
    while a hook may still be reading it — so if we do not remove it, only the
    gateway's retention sweep will, hours later.
    """
    if cfg is not None and not cfg.get("audio", "delete_after", True):
        return
    try:
        audio.unlink(missing_ok=True)
    except OSError as e:
        log(f"[cleanup] could not delete {audio}: {e}")


def main() -> int:
    audio_path = os.environ.get("XIAOMI_AUDIO_FILE") or ""
    if not audio_path and not sys.stdin.isatty():
        # The gateway also hands the payload over on stdin; accept either.
        try:
            audio_path = json.loads(sys.stdin.read() or "{}").get("audio_file") or ""
        except json.JSONDecodeError:
            pass
    if len(sys.argv) > 1:
        audio_path = sys.argv[1]  # manual run: pipeline.py some.wav

    audio = Path(audio_path)
    if not audio_path or not audio.is_file():
        log(f"[error] no audio file (XIAOMI_AUDIO_FILE={audio_path!r})")
        return 1

    cfg: Config | None = None
    try:
        cfg = load_config()
        code = run(audio, cfg)
    except Fail as e:
        log(f"[error] {e}")
        if cfg is not None:
            notify(cfg, "语音处理失败", str(e))
        # A failed run keeps its audio: it is the only copy, and it is what you
        # need to reproduce the failure by hand.
        return 1

    cleanup(audio, cfg)
    return code


if __name__ == "__main__":
    sys.exit(main())
