# 项目详细说明

本项目将小米蓝牙遥控器 2 Pro（RC003 / ARN9 普通款）的麦克风接入 Linux：按住语音键录音，松开后生成 WAV，并将文件路径交给外部 Hook 脚本。

Rust 网关负责蓝牙连接、语音解码、录音和事件通知。语音识别（ASR）、LLM、剪贴板和其他业务由 Hook 实现；使用网关本身不需要配置云服务或 API Key。

## 目录

- [安装与首次运行](#安装与首次运行)
- [配置与命令行](#配置与命令行)
- [录音流程与音频格式](#录音流程与音频格式)
- [Hook 接口](#hook-接口)
- [录音文件与自动清理](#录音文件与自动清理)
- [示例选择](#示例选择)
- [日常使用：语音识别与剪贴板](#日常使用语音识别与剪贴板)
- [安装为 systemd 用户服务](#安装为-systemd-用户服务)
- [麦克风键与其他按键](#麦克风键与其他按键)
- [日志与排查](#日志与排查)
- [技术实现与开发验证](#技术实现与开发验证)

## 安装与首次运行

需要 Linux、可用的蓝牙适配器与系统蓝牙服务（BlueZ），以及 Rust 1.89 或更新版本和本机编译工具链。网关没有图形界面，先在系统蓝牙设置中完成遥控器配对。

以下命令均从仓库根目录执行：

```bash
cargo build --release
cp config.example.jsonc config.jsonc
./target/release/xiaomi-remote-2pro-minimal --config ./config.jsonc
```

[示例配置](config.example.jsonc)默认使用打印录音信息的 end Hook，不会调用 ASR。按住麦克风键说话，松开后检查日志中的录音路径和 Hook 输出。按 Ctrl-C 结束。

只想录音时，可以不传配置：

```bash
./target/release/xiaomi-remote-2pro-minimal
```

网关不会自动加载当前目录的配置文件。需要指定配置时，必须传入 `--config`。

可以单独检查设备发现：

```bash
./target/release/xiaomi-remote-2pro-minimal scan --seconds 5
```

扫描用于查看已连接的 ATVV 设备及附近广播，不替代系统蓝牙配对。网关会自动发现目标遥控器，连接中断后自动重试。

## 配置与命令行

网关接受 JSON 或 JSONC。JSONC 支持 `//`、`/* */` 注释，不支持尾逗号。配置只包含网关设置，未知字段会报错；不要把 ASR、LLM 或 API Key 配置放在这里。

```jsonc
{
  "record_dir": "./recordings",
  "gain": 0,
  "voice_key": "auto",
  "keep_hours": 24,
  "keep_max": 100,
  "hooks": {
    "start": "./hooks/on_start.sh",
    "end": ["./hooks/on_end.py"]
  }
}
```

上面的脚本路径是自定义示例，使用前需要创建对应文件。配置中的相对脚本路径、`record_dir` 均以配置文件所在目录为基准；CLI 中的相对路径以启动命令的当前目录为基准。

| 配置字段 | CLI 参数 | 默认值与说明 |
| --- | --- | --- |
| `record_dir` | `--record-dir` | 系统临时目录下的 `xiaomi-remote-gateway` |
| `gain` | `--gain` | `0`；数字增益，范围 -24～24 dB |
| `device_name` | `--device-name` | 不指定；按设备名称子串进一步过滤候选设备 |
| `voice_key` | `--voice-key` | `auto`；可选 `auto` / `always` / `never` |
| `reconnect` | `--reconnect` | `3`；重连基础间隔，秒，实际最小为 1 秒；连续失败会退避 |
| `keep_hours` | `--keep-hours` | `24`；超过保留时长的录音可被删除，`0` 关闭时间限制 |
| `keep_max` | `--keep-max` | `100`；保留最新录音的数量，`0` 关闭数量限制 |
| `hooks` | `--hook`、`--hook-config` | 不配置则只录音；详见 Hook 接口 |

普通参数以显式传入的 CLI 值优先于配置文件。Hook 则会合并注册，按 `--hook-config` → `--config` → `--hook` 的顺序追加，不会覆盖同事件已有脚本。

其他命令行选项：

| 选项 / 子命令 | 用途 |
| --- | --- |
| `run` | 启动网关；省略子命令时默认执行 |
| `scan --seconds 5` | 扫描；省略 `--seconds` 时为 10 秒 |
| `--debug` | 开启网关全部诊断日志，包括 TRACE；`-v` / `--verbose` 为别名 |
| `--log-file ./gateway-debug.log` | 日志同时追加到文件和终端 |
| `--help` / `--version` | 查看帮助 / 版本 |

例如，临时覆盖增益：

```bash
./target/release/xiaomi-remote-2pro-minimal --config ./config.jsonc --gain 6
```

## 录音流程与音频格式

```text
按住麦克风键 → ATVV 语音流开启 → start Hook
    ↓
接收 ADPCM 音频 → 解码、平滑与增益 → 写入 WAV
    ↓
松开麦克风键 → 结束录音 → 有效音频触发 end Hook
    ↓
Hook 自行转写、复制、上传、移动或删除文件
```

- 输出为单声道、16 kHz、16-bit PCM WAV，文件名为 `record_<时间戳>.wav`。
- `start` 在语音流开启时触发，此时尚无可交付的音频文件。
- `end` 仅在录音结束并生成有效音频后触发。少于 800 个采样点（50 ms）的录音会直接删除，不触发 end Hook，因此 start 与 end 不保证一一配对。
- Hook 的 `duration_sec` 是按采样数计算的音频时长，不是按键的墙钟耗时。
- 异常断线时会尝试收尾已收到的音频；是否触发 end 仍取决于是否生成有效录音。

## Hook 接口

### 注册脚本

最常用的方式是在 `--config` 指定的文件中配置 `hooks`。每个事件接受单个脚本路径或路径数组。

也可以直接使用命令行，省略事件名时默认为 `end`：

```bash
./target/release/xiaomi-remote-2pro-minimal \
  --hook start=./notify.sh \
  --hook end=./transcribe.py \
  --hook ./upload.sh
```

兼容的 `--hook-config` 专门加载 Hook 配置，当前示例路径为：

```bash
./target/release/xiaomi-remote-2pro-minimal \
  --hook-config ./examples/1.hooks-print-log/hooks.jsonc
```

脚本需有可执行权限和正确的 shebang（例如 `#!/usr/bin/env python3`）：

```bash
chmod +x ./transcribe.py
```

网关直接执行脚本文件，不将配置字符串当作 shell 命令解释。需要传固定参数或组合命令时，写一个包装脚本。Hook 的工作目录是脚本所在目录。

同一事件的多个脚本独立并发执行，不保证开始或完成顺序。慢脚本或执行失败不会阻塞后续录音；网关没有为 Hook 设置超时、自动重试或串行队列。多个 end Hook 共用同一音频文件时，应自行协调，避免其中一个提前删除文件。

### 事件数据

每个 Hook 同时收到 stdin JSON 与 `XIAOMI_*` 环境变量，任选一种读取即可。

end 的 stdin 示例：

```json
{
  "event": "end",
  "audio_file": "/tmp/xiaomi-remote-gateway/record_20260101_120000.000.wav",
  "duration_sec": 1.5,
  "samples": 24000,
  "sample_rate": 16000
}
```

start 的 JSON 保留相同字段：`audio_file` 为 `null`，`duration_sec` 和 `samples` 为 `0`，`sample_rate` 为 `16000`。

| 环境变量 | 含义 | 事件 |
| --- | --- | --- |
| `XIAOMI_EVENT` | `start` 或 `end` | 全部 |
| `XIAOMI_SAMPLE_RATE` | 固定 `16000` | 全部 |
| `XIAOMI_AUDIO_FILE` | WAV 文件绝对路径 | end |
| `XIAOMI_DURATION_SEC` | 音频秒数，保留三位小数 | end |
| `XIAOMI_SAMPLES` | PCM 采样点数 | end |

最小 end Hook 示例：

```sh
#!/bin/sh
set -eu
printf '录音：%s，时长：%s 秒\n' "$XIAOMI_AUDIO_FILE" "$XIAOMI_DURATION_SEC"
# 在这里处理音频；确认消费成功后再移动或删除文件。
```

Hook stdout 以 INFO 级别写入网关日志，stderr 和非零退出状态以 WARN 记录，启动失败以 ERROR 记录。

## 录音文件与自动清理

录音主要由 end Hook 消费。没有配置 Hook、Hook 失败或未主动删除的文件会保留，直到满足清理条件。

| 时机 | 行为 |
| --- | --- |
| 网关启动 | 清理目录内符合保留策略的旧录音 |
| 有效录音结束 | 触发 end Hook 后清理旧录音，本次刚交付的文件受保护 |
| 录音短于 50 ms | 直接删除该文件，不交给 end Hook |

默认超过 24 小时或排在最新 100 个之外的文件可被删除。清理根据文件修改时间排序，只匹配录音目录下的 `record_*.wav` 文件，不处理其他名称的文件；请避免将无关音频命名为这一格式。

```bash
# 完全关闭保留策略清理；过短录音仍会丢弃
./target/release/xiaomi-remote-2pro-minimal --keep-hours 0 --keep-max 0
```

清理由启动和录音结束触发，不是后台定时任务。本次录音仅在本轮清理中受保护；耗时较长的 Hook 应先复制音频，或关闭保留策略清理，以免后续录音或重启时文件被清除。

## 示例选择

| 示例 | 用途 |
| --- | --- |
| [1.hooks-print-log](examples/1.hooks-print-log/) | 打印 start / end 事件，验证链路 |
| [2.hooks-exec-command](examples/2.hooks-exec-command/) | 查看在 Hook 中执行自定义命令的方式 |
| [3.hooks-asr-and-copy](examples/3.hooks-asr-and-copy/) | 简单 Python ASR → Wayland 剪贴板，读取 `ALIBABA_API_KEY` 环境变量 |
| [4.daily-use](examples/4.daily-use/) | 日常使用：ASR → 剪贴板、加密凭据、API 用量记录和 systemd 用户服务 |

前三个示例均可通过 `--config examples/<示例目录>/hooks.jsonc` 运行。第三个示例需要 Python 3、`wl-copy` 和用户自行提供的 API Key，其日志会打印识别文本。

旧 `examples/real-use-systemd/` 目前仅残留旧说明，相关安装器和流水线已在重构中删除。部署请使用 `4.daily-use`；当前日常示例没有内置 LLM 步骤，需要时自行扩展 end Hook。

## 日常使用：语音识别与剪贴板

此可选示例需要 Python 3、OpenSSL，以及 `wl-clipboard`（Wayland）或 `xclip`（X11）。API 调用与费用由用户配置的服务决定。

```bash
python3 examples/4.daily-use/hooks/credential-manage.py init
python3 examples/4.daily-use/hooks/credential-manage.py set
./target/release/xiaomi-remote-2pro-minimal --config examples/4.daily-use/config.jsonc
```

`set` 会隐藏输入 API Key。Hook 配置位于同目录生成的 `hooks-config.jsonc`，模板为 [hooks-example.config.jsonc](examples/4.daily-use/hooks/hooks-example.config.jsonc)。按当前脚本默认值，使用 DashScope 原生多模态接口和 `qwen-audio-3.1-asr-flash`，可在 Hook 配置中覆盖 `endpoint`、`model`。这些是示例实现的默认设置，实际可用模型与账号权限以所使用的服务为准。

end Hook 每次调用读取配置，提交 WAV，解析识别结果并写入剪贴板；成功后删除录音。空结果或失败时保留录音，仍受网关清理策略约束。复制完成后自行按 Ctrl-V 粘贴。

凭据命令均使用相同脚本：

| 命令 | 用途 |
| --- | --- |
| `init` | 配置不存在时从示例初始化 |
| `set` | 隐藏输入并加密保存 API Key |
| `encrypt` | 将配置中已有的明文 API Key 转为加密凭据 |
| `status` | 查看配置状态，不输出密钥明文 |
| `delete` | 移除 API Key |

配置保存 `${secret:api_key}` 引用；密文与本地密钥分别存放在 `credentials.enc` 和 `credentials.key`，权限均为 0600。能读取两者的本机用户仍可解密。

### API 用量记录

日常示例将每次调用追加到 Hook 目录下的 `api-usage.jsonl`，权限为 0600。记录 UTC 时间、模型、请求状态、提交音频时长、请求耗时，以及 API 返回的 `usage`、`request_id`，不保存音频内容、识别文本或 API Key。统计写入失败不影响剪贴板处理。

```bash
python3 examples/4.daily-use/hooks/api-usage-stats.py
```

统计逐项累计 API 返回的数值字段，保留嵌套字段路径；没有返回的字段不推算。总 token 与明细分别展示，不重复相加。提交音频秒数按 WAV 计算，包括失败调用和重试；请求耗时单独统计。收到响应后即记录，后续解析或复制失败不影响已记录的 API 用量。统计从启用记录时开始，不能补回历史调用，也不计算账单金额。

## 安装为 systemd 用户服务

从仓库运行以下命令，默认会构建 release、安装程序并启用和重启用户服务：

```bash
examples/4.daily-use/install-service.sh install
examples/4.daily-use/install-service.sh paths
```

安装前先停止前台网关。同一 Linux 用户只允许一个当前版本的网关录音实例运行。

默认安装位置如下，设置 `XDG_DATA_HOME` / `XDG_CONFIG_HOME` 时遵循对应目录：

| 内容 | 默认位置 |
| --- | --- |
| 程序、网关配置和 Hook | `~/.local/share/xiaomi-remote-gateway/` |
| 服务单元 | `~/.config/systemd/user/xiaomi-remote-gateway.service` |
| 已安装的 Hook 配置 | `~/.local/share/xiaomi-remote-gateway/hooks/hooks-config.jsonc` |

仓库中的凭据不会复制到安装目录。根据 `paths` 的输出，使用已安装的凭据脚本重新设置 API Key，默认路径下命令为：

```bash
python3 ~/.local/share/xiaomi-remote-gateway/hooks/credential-manage.py set
```

管理命令：

```bash
examples/4.daily-use/install-service.sh status
examples/4.daily-use/install-service.sh logs
examples/4.daily-use/install-service.sh restart
examples/4.daily-use/install-service.sh update
examples/4.daily-use/install-service.sh update --no-build
examples/4.daily-use/install-service.sh uninstall
```

`update` 更新二进制、Hook 程序和服务模板并重启，保留已安装的 `config.jsonc`、`hooks/hooks-config.jsonc` 与凭据。`--no-build` 复用仓库中已有的 release 二进制。`uninstall` 停用并移除服务单元，保留程序、配置与凭据。

安装后的服务使用安装目录内的文件，修改仓库配置不会直接影响它。修改已安装的网关配置后重启服务；Hook 配置在每次调用时重新读取。

安装器会把当前图形会话的显示相关变量同步到用户级 systemd。若服务能够识别但无法复制，可在当前图形终端重新运行安装或更新命令。服务日志通过 `logs` 查看；已安装版本的用量统计默认使用：

```bash
python3 ~/.local/share/xiaomi-remote-gateway/hooks/api-usage-stats.py
```

## 麦克风键与其他按键

语音通过 BLE 的 ATVV 通道接收；遥控器同时作为标准蓝牙键盘发送按键。麦克风键可能发送 F5 / F20 等键码，触发桌面快捷键，因此网关提供麦克风键拦截：

| `voice_key` | 行为 |
| --- | --- |
| `auto` | 默认；检测到 keyd 等重映射器时让位，否则尝试独占遥控器输入并通过 uinput 转发非麦克风按键 |
| `always` | 强制尝试接管输入；仍受内核独占与设备权限限制 |
| `never` | 不处理输入层，交给用户自行管理 |

输入层需要读取遥控器的 evdev 设备并写入 `/dev/uinput`。仓库提供一次性的权限设置脚本：

```bash
sudo ./scripts/setup-uinput.sh
```

该脚本会加载 uinput、写入模块加载配置和 udev 规则，并将用户加入 `input` 组。执行后需重新登录使组权限生效；加入 `input` 组会扩大对本机输入设备的访问权限。

其他按键映射交给 keyd 等工具。使用 keyd 时，可参考 [按键配置示例](examples/4.daily-use/keyd-example/xiaomi-remote.conf)，按自己的设备与需求调整，并自行安装和应用；服务安装器不会处理 keyd。`auto` 让位后，应由重映射器将麦克风键映射为 `noop`，避免按键继续传到桌面。

## 日志与排查

默认网关日志为 INFO 及以上，底层依赖保留 WARN 及以上。日志包含毫秒时间戳、级别和模块。

| 级别 | 主要内容 |
| --- | --- |
| ERROR | 录音写入、协议或 Hook 启动失败 |
| WARN | 断线、Hook 非零退出、脚本 stderr 和音频时长异常 |
| INFO | 连接状态、录音文件、音频统计和脚本 stdout |
| DEBUG | 控制收发、同步恢复、Hook PID、耗时与退出结果 |
| TRACE | 逐包长度、解码数量、包指纹和丢弃原因 |

```bash
./target/release/xiaomi-remote-2pro-minimal \
  --config ./config.jsonc --debug --log-file ./gateway-debug.log
```

日志同时输出到终端 stderr 与文件。文件不存在时创建，存在时追加，父目录需已存在；不自动轮转。文件日志不带终端颜色，scan 的设备列表输出到 stdout，不包含在日志文件中。

常见问题：

- **发现不了遥控器**：确认蓝牙已开启、遥控器已配对并唤醒，使用 `scan` 检查设备；配置了 `device_name` 时检查过滤条件。
- **提示已有实例**：停止之前的前台进程或用户服务。锁会随进程退出由内核释放，无需删除锁文件；旧版本没有锁，仍需自行停止。
- **有 start，没有 end**：检查是否只是小于 50 ms 的误触，再检查语音结束、断线及 WAV 写入日志。
- **录音正常，Hook 启动失败**：检查脚本路径、可执行权限、shebang，以及解释器是否可用。
- **按语音键触发桌面快捷键**：检查输入设备权限、`voice_key` 模式，以及 keyd 是否已负责屏蔽麦克风键。
- **ASR 或复制失败**：查看 Hook 的阶段日志；服务部署需检查已安装的凭据和图形会话变量。
- **音频时长明显超过实际说话时长**：检查是否还有旧网关或服务运行，再查看 DEBUG 的实际耗时、采样时长、音频包计数与重复包统计。相同包内容可能合法，网关不会仅凭内容相同丢包。

分享诊断信息前先检查内容：设备扫描和连接日志可能带有设备标识，Hook 输出可能包含识别文字。不要提交个人录音、凭据、本地配置或含私人信息的日志。

## 技术实现与开发验证

- 使用 Google Android TV Voice-over-BLE（ATVV）GATT 协议进行语音协商和接收。
- 内置 IMA/DVI ADPCM 解码，保留语音流内的解码状态。
- PCM 经过 `(1, 2, 1) / 4` 三抽头平滑与可调数字增益后写入 WAV。
- 自动发现已连接或广播的候选遥控器，断线后重建语音会话并重连。
- Linux 输入层使用 evdev 独占与 uinput 转发，并在自动模式下为已有重映射器让位。

源码按职责分为蓝牙桥接、ATVV 会话、解码与音频处理、录音、Hook、保留策略和输入键处理。Python 日常示例与 Rust 网关独立，替换 Hook 无需修改网关。

修改实现后，可运行仓库现有检查：

```bash
cargo test
python3 -m unittest discover -s tests -p 'test_*.py'
```

自动测试不替代真实遥控器、桌面剪贴板和用户服务的实际验证。
