# real-use-systemd — 按一下说话，文字进剪贴板

把 Gateway 装成 systemd 用户服务，并接上一条完整流水线：

```
遥控器麦克风键 ──► Gateway 录音 (16kHz WAV)
                        │  end hook
                        ▼
              ① 阿里云 ASR（百炼 / DashScope，OpenAI 兼容）
                        │  原始转写
                        ▼
              ② OpenAI-compatible LLM 后处理（补标点、去口水词）
                        │
                        ▼
              ③ 剪贴板 + 桌面通知（+ 可选 JSONL 日志 / 自定义命令）
```

> 确实，这个目录已经不太像"example"了。所以它被做成了一个能被反复执行的安装器：
> 一个配置文件、一个凭据库、一条 `install-service.sh` 命令，**所有操作都是 upsert**。

---

## 60 秒上手

```bash
cd examples/real-use-systemd

./install-service.sh                       # 编译 + 生成配置 + 安装并启动服务
./install-service.sh secret add dashscope_api_key sk-xxxxxxxx
./install-service.sh secret add llm_api_key       sk-xxxxxxxx
./install-service.sh restart

./install-service.sh doctor                # 从头到尾体检
```

API Key 在 [阿里云百炼控制台](https://bailian.console.aliyun.com/) 获取；两步可以共用同一个 Key。

---

## 命令一览

| 命令 | 作用 |
| --- | --- |
| `./install-service.sh` / `install` | 编译 → 安装/更新 → 启动（默认子命令） |
| `install --no-build` | 跳过 `cargo build`，复用现有二进制 |
| `secret add <名字> [值]` | 写入凭据；**省略值则交互式输入**，不进 shell history |
| `secret list` / `secret rm <名字>` | 只列名字，永远不打印值 |
| `config init [--force]` / `config show` / `config path` | 配置文件 |
| `status` / `logs [-f]` / `restart` | 日常运维 |
| `test ./some.wav` | 用一个 WAV 手动跑一遍 hook，调 prompt 时很有用 |
| `doctor` | 检查 Python / openssl / 配置 / 凭据 / 二进制 / 服务 / 剪贴板工具 |
| `uninstall [--purge]` | 停用并删除服务；`--purge` 连安装产物一起删 |

只依赖 **Python 3.11+（stdlib `tomllib`）+ openssl**，不需要 pip、venv 或任何第三方包。

---

## 装到哪里

| 路径 | 内容 |
| --- | --- |
| `~/.config/xiaomi-remote-gateway/config.toml` | 唯一的配置文件（手写） |
| `~/.config/xiaomi-remote-gateway/secrets.enc` | 加密后的凭据库（0600） |
| `~/.config/xiaomi-remote-gateway/secrets.key` | 本地主密钥（0600） |
| `~/.local/share/xiaomi-remote-gateway/bin/` | 二进制副本 |
| `~/.local/share/xiaomi-remote-gateway/hook/` | `pipeline.py` + `common.py` + `hooks.json` |
| `~/.config/systemd/user/xiaomi-remote-gateway.service` | 生成的 unit（**不要手改**） |

二进制和 hook 都是**拷贝进去**的：装完以后 `git clean`、切分支、甚至把整个仓库挪走，
正在跑的服务都不受影响。

### upsert 语义

每个文件都先比对内容、不同才写，然后：`daemon-reload` → 没 enable 就 enable →
没跑就 start / 有变化才 restart。所以**什么都没改时重跑安装不会打断服务**：

```
==> changed: nothing (already up to date)
==> xiaomi-remote-gateway.service already running with this configuration
```

改了 `[asr]` `[llm]` `[output]` `[audio]` 甚至**不用重启**——hook 每次执行都会重新读配置。
只有 `[service]` `[gateway]` 影响 unit，需要重跑一次 `install`。

---

## 配置为什么是 TOML

- `tomllib` 是 Python 3.11+ 的**标准库**，零依赖；YAML 得拖一个 PyYAML，每台装服务的机器都要装。
- 和 JSON 不同，TOML **能写注释**——一个供人手改的配置文件，注释是刚需。
- 多行字符串（`"""..."""`）刚好用来放 LLM 的 system prompt。

完整带注释的模板见 [`config.example.toml`](config.example.toml)。

---

## 凭据是怎么存的

配置文件里**不写明文**，只写占位符：

```toml
[asr]
api_key = "${secret:dashscope_api_key}"
```

值放进加密库：

```bash
./install-service.sh secret add dashscope_api_key sk-xxxx
./install-service.sh secret add dashscope_api_key          # 不带值 → 交互式输入
```

实现：整个库是一个 JSON 对象，用 `openssl enc -aes-256-cbc -pbkdf2 -iter 600000` 整体加密，
密钥是 `secrets.key` 里的 32 字节随机数（0600，权限不对会被自动收紧）。
口令通过**专用管道 fd** 交给 openssl，不走 argv（`/proc` 里全世界可见）、也不走环境变量。
写入是 `tmp` + `rename` 的原子替换。

**这层防护的边界，说清楚**：它挡的是凭据以明文躺在文件里——被备份收走、被 `grep -r` 翻到、
录屏时晃出来、手滑 `git add` 进去。它**挡不住**已经能以你的身份读文件的人——那人同样能读 `secrets.key`。
真正的封装需要 TPM（`systemd-creds encrypt --with-key=host`），但它要 root，
当前 systemd 的用户级服务用不了，所以这里没用它。装成系统级服务的话它是更好的选择。

`secret list` 只打印名字；任何命令都不会把值打到终端或日志里。

---

## 和 keyd 的关系（重要）

麦克风键除了走 BLE，还会作为普通 HID 键（本机是 `F5` / `F20`）发给桌面。屏蔽它有两条路，
**同时只能有一条生效**，因为 `EVIOCGRAB` 是独占的：

1. **keyd（推荐）**：现成的参考配置在 [`keyd/xiaomi-remote.conf`](keyd/xiaomi-remote.conf)。

   ```bash
   sudo cp keyd/xiaomi-remote.conf /etc/keyd/ && sudo keyd reload
   ```

   分工就是「**语音键继续走语音，其余按键交给 keyd**」：麦克风键的 BLE ATVV 语音流和它的
   HID 键是两条独立通路，keyd 把 HID 那条 `noop` 掉完全不影响录音。

   > ⚠️ 麦克风键**同时发 `f5` 和 `f20`**，两个都要 `noop`。只写 `f5` 的话 `f20` 仍会漏到桌面——
   > 多数程序不理会它，但 fcitx5 / GNOME 自定义快捷键是可以绑上去的。

   配置里保持 `voice_key = "auto"`（默认值）即可。Gateway 会探测到 `/run/keyd.socket`，
   主动让位、完全不碰输入层，并在日志里说明：

   ```
   [VOICE KEY] keyd is running (/run/keyd.socket); leaving the remote's keys to it.
   ```

2. **Gateway 自己抓**：`voice_key = "always"`，需要先跑一次 `sudo ../../scripts/setup-uinput.sh`。
   只在没装 keyd 之类重映射器时才这么配。

`doctor` 会检查这两者是否打架。原理细节见主 README 的「屏蔽麦克风键」一节。

---

## 会话环境（为什么"终端里好用、当服务就没反应"）

`wl-copy` / `notify-send` 需要 `WAYLAND_DISPLAY` 和会话 D-Bus。用户级 unit 只有在登录时
把这些变量导入过 user manager 才看得到，所以安装器会替你执行
`systemctl --user import-environment WAYLAND_DISPLAY DISPLAY ...`。

服务跟随桌面会话：**注销即停止**。要让它在无人登录时也跑：

```bash
sudo loginctl enable-linger $USER
```

（但那时剪贴板/通知没有会话可用，通常配合 `[output] command = [...]` 走别的出口。）

---

## 日志去哪了、会不会撑爆磁盘

**没有自己的日志文件。** Gateway 只往 stderr 写（`env_logger`），hook 脚本的 stdout/stderr 也由
Gateway 转发进自己的日志，所以整条链路最终都落在 **journald**：

```bash
./install-service.sh logs -f                       # 等价于 journalctl --user -u ...
journalctl --user -t xiaomi-remote-gateway --since today
```

| 东西 | 上限 | 谁在管 |
| --- | --- | --- |
| 服务日志 | journald `SystemMaxUse`：文件系统的 10%，最多 4G，自动 rotate + vacuum | journald |
| 录音 WAV | `[gateway] keep_hours` / `keep_max`（默认 24 小时 / 100 个） | Gateway 自己 |
| `transcript_log` JSONL | `transcript_log_max_bytes`（默认 1 MiB），超了就转成 `.jsonl.1`，只留一代 | `pipeline.py` |

所以都不是无限的。两个实际注意点：

- **journald 默认上限不小**。看当前占用：`journalctl --disk-usage`；要压下去：
  `sudo journalctl --vacuum-size=500M`，或写死上限：

  ```ini
  # /etc/systemd/journald.conf.d/size.conf
  [Journal]
  SystemMaxUse=500M
  ```

- **`[gateway] verbose = true` 别长期开**。journald 默认限流 `RateLimitBurst=10000 / 30s`，
  刷得太快的消息会被直接丢掉——排查完就关掉。

`doctor` 会顺带把当前 journal 占用打出来。

---

## 调 prompt 的正确姿势

不用每次都对着遥控器说话——留一个 WAV，直接反复跑：

```bash
./install-service.sh test ~/sample.wav
```

它以和服务完全一致的方式（同样的环境变量、同样的脚本）执行 hook，日志直接打在终端。
注意：**成功**的运行会按 `[audio] delete_after` 删掉音频，想留着就先 `cp` 一份；
**失败**的运行永远保留音频，那是复现问题唯一的样本。

---

## 排错

```bash
./install-service.sh doctor          # 先跑这个
./install-service.sh logs -f         # hook 的 stdout/stderr 都会进 journal
```

| 现象 | 原因 |
| --- | --- |
| `missing secrets: ...` | 占位符在配置里，值还没进加密库 |
| `HTTP 401` | Key 不对，或 `base_url` 与 Key 来源不匹配 |
| `HTTP 400 ... language` | `[asr] language` 写成了 `zh-CN`，要用 `zh` |
| 识别正常但剪贴板空 | 缺 `wl-clipboard` / `xclip`，或会话变量没导入（见上一节） |
| 麦克风键还是触发了 VS Code 调试 | keyd 里没把 `f5`/`f20` 映射成 `noop` |
| `[VOICE KEY] ... EBUSY` | keyd 和 `voice_key = "always"` 在抢设备，改回 `auto` |
