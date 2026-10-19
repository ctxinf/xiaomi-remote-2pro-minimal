

## Hook 配置

### 方式一：命令行参数 `--hook`

```bash
# 指定事件
--hook end=./transcribe.py
--hook start=./notify.sh

# 省略事件名时默认为 end（携带音频文件的事件，最常用）
--hook ./transcribe.py

# 可重复传入，多个脚本独立启动，不保证执行或完成顺序
--hook start=./beep.sh --hook end=./transcribe.py --hook end=./upload.js
```

### 方式二：JSONC 配置文件 `--hook-config`

```bash
./target/release/xiaomi-remote-pro2-gateway --hook-config ./examples/hooks/1.basic/hooks.jsonc
```

```json
{
  "hooks": {
    "start": "./on_start.sh",
    "end": ["./transcribe.py", "./upload.js"]
  }
}
```

- 每个事件可以是单个字符串，或字符串数组。
- **相对路径以配置文件所在目录为基准**，方便配置和脚本一起移动。
- 两种方式可同时使用：配置文件中的 hook 先注册，命令行 `--hook` 后注册；脚本并发执行，不保证完成顺序。

---

## Hook 脚本接口

脚本可以是任意可执行文件（shell / Python / Node ...），需具备可执行权限与 shebang：

```bash
chmod +x ./my_hook.sh
```

事件数据通过**两种方式**同时传入，取用任选其一：

**1. stdin 上的 JSON**

```json
{
  "event": "end",
  "audio_file": "/tmp/xiaomi-remote-gateway/record_20260824_101530.123.wav",
  "duration_sec": 1.830,
  "samples": 29280,
  "sample_rate": 16000
}
```

**2. 环境变量**

| 变量 | 说明 | 事件 |
| --- | --- | --- |
| `XIAOMI_EVENT` | `start` 或 `end` | 全部 |
| `XIAOMI_SAMPLE_RATE` | 采样率（Hz），固定 `16000` | 全部 |
| `XIAOMI_AUDIO_FILE` | 音频文件绝对路径 | 仅 `end` |
| `XIAOMI_DURATION_SEC` | 录音时长（秒） | 仅 `end` |
| `XIAOMI_SAMPLES` | 采样点总数 | 仅 `end` |


ASR、LLM 和任何网络 API 调用全部由脚本实现。网关不读取模型或 API 密钥设置。
完整语音处理示例见 [systemd Hook 示例](../examples/real-use-systemd/README.md)。
JSONC 支持注释，不支持尾逗号；旧 JSON 仍可用。
