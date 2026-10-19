#!/usr/bin/env python3
"""录音 → 阿里云语音识别 → Wayland 剪贴板。需要 python3 和 wl-copy。"""

import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


def main():
    # 唯一需要用户设置的变量；其余参数固定，方便阅读和修改。
    api_key = os.environ.get("ALIBABA_API_KEY")
    if not api_key:
        raise ValueError("请先 export ALIBABA_API_KEY=你的API密钥")
    if not shutil.which("wl-copy"):
        raise ValueError("请先安装 wl-clipboard（提供 wl-copy 命令）")
    audio_file = os.environ.get("XIAOMI_AUDIO_FILE")
    if not audio_file:
        raise ValueError("缺少 XIAOMI_AUDIO_FILE，请通过网关的 end hook 运行")

    # 将 WAV 编码后提交给 DashScope 的 OpenAI 兼容接口。
    audio = Path(audio_file)
    encoded = base64.b64encode(audio.read_bytes()).decode("ascii")
    body = {
        "model": "qwen3-asr-flash",
        "stream": False,
        "asr_options": {"enable_itn": False},
        "messages": [{
            "role": "user",
            "content": [{
                "type": "input_audio",
                "input_audio": {"data": f"data:audio/wav;base64,{encoded}"},
            }],
        }],
    }
    request = Request(
        "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
    )
    with urlopen(request, timeout=60) as response:
        result = json.load(response)

    # 提取识别文字；空结果保留录音，方便重试。
    text = result["choices"][0]["message"]["content"]
    if isinstance(text, list):
        text = "".join(part.get("text", "") for part in text)
    text = text.strip()
    if not text:
        print("没有识别到文字")
        return

    # wl-copy 后台持有剪贴板，输出重定向以免占用网关的日志管道。
    subprocess.run(
        ["wl-copy"], input=text.encode("utf-8"), check=True,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    print(f"已复制，按 Ctrl+V 粘贴：{text}")
    audio.unlink()


if __name__ == "__main__":
    try:
        main()
    except HTTPError as error:
        print(f"语音识别请求失败：HTTP {error.code}", file=sys.stderr)
        sys.exit(1)
    except (ValueError, KeyError, IndexError, TypeError, OSError, URLError,
            subprocess.CalledProcessError) as error:
        print(f"识别或复制失败：{error}", file=sys.stderr)
        sys.exit(1)
