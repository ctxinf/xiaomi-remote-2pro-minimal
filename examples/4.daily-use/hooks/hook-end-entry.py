#!/usr/bin/env python3
"""end 入口：录音 → DashScope ASR → 剪贴板。需要 Python 3、wl-copy 或 xclip。"""
import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from hooks_common import Fail, api_key, read_config
from api_usage import record_call


def transcribe(audio, config):
    encoded = base64.b64encode(audio.read_bytes()).decode("ascii")
    body = {
        "model": config["model"],
        "input": {"messages": [{
            "role": "user",
            "content": [{"type": "input_audio", "input_audio": {
                "data": f"data:audio/wav;base64,{encoded}",
            }}],
        }]},
        "parameters": {"format": "wav", "sample_rate": "16000"},
    }
    request = Request(config["endpoint"], data=json.dumps(body).encode(), headers={
        "Authorization": f"Bearer {api_key(config)}",
        "Content-Type": "application/json",
        "X-DashScope-SSE": "disable",
    })
    started = time.perf_counter()
    try:
        with urlopen(request, timeout=60) as response:
            result = json.load(response)
    except Exception as error:
        record_call(audio, config["model"], time.perf_counter() - started, error=error)
        raise
    record_call(audio, config["model"], time.perf_counter() - started, result=result)
    # 当前 ASR 返回 output.text；同时兼容多模态 choices 格式。
    output = result.get("output") if isinstance(result, dict) else None
    if not isinstance(output, dict):
        raise Fail("ASR 响应缺少 output 对象")
    if "text" in output:
        text = output["text"]
    else:
        try:
            text = output["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError) as error:
            raise Fail("ASR 响应缺少 output.text 或 choices.message.content") from error
        if isinstance(text, list):
            text = "".join(part.get("text", "") for part in text)
    if not isinstance(text, str):
        raise Fail("ASR 返回的识别文本不是字符串")
    return text.strip()


def copy_text(text):
    for command in (["wl-copy"], ["xclip", "-selection", "clipboard"]):
        if shutil.which(command[0]):
            subprocess.run(command, input=text.encode(), check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return
    raise Fail("请安装 wl-clipboard（Wayland）或 xclip（X11）")


def main():
    audio_file = os.environ.get("XIAOMI_AUDIO_FILE")
    if not audio_file:
        raise Fail("缺少 XIAOMI_AUDIO_FILE，请通过网关的 end hook 运行")
    audio = Path(audio_file)
    print("[hook:end] ASR request started", flush=True)
    text = transcribe(audio, read_config())
    print("[hook:end] ASR response parsed", flush=True)
    if not text:
        print("[hook:end] 未识别到文字，保留录音")
        return
    print("[hook:end] copying transcript to clipboard", flush=True)
    copy_text(text)
    print("[hook:end] 已复制到剪贴板，按 Ctrl+V 粘贴", flush=True)
    audio.unlink()


if __name__ == "__main__":
    try:
        main()
    except HTTPError as error:
        print(f"[hook:end] ASR 请求失败：HTTP {error.code}", file=sys.stderr)
        sys.exit(1)
    except Fail as error:
        print(f"[hook:end] {error}；录音保留", file=sys.stderr)
        sys.exit(1)
    except (OSError, ValueError, KeyError, IndexError, TypeError,
            subprocess.CalledProcessError) as error:
        print(f"[hook:end] 识别或复制失败（{type(error).__name__}），查看上一条阶段日志；录音保留", file=sys.stderr)
        sys.exit(1)
