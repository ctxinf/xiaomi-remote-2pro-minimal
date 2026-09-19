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
| `~/.local/share/xiaomi-remote-gateway/hook/` | `on_record.py` + `pipeline.py` + `common.py` + `hooks.json` |
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

## 屏幕上的状态浮层

整条链路里最难受的不是慢，是不知道自己现在处在哪一步：按下去没反应，到底是遥控器睡了、
服务挂了，还是已经在录了？所以从**按下按键那一刻**就上屏，一路改到结果：

```
按下按键 ──录音中──> 松开按键 ──ASR──> 出结果
   │                    │                │
   ↓                    ↓                ↓
"正在录音…"    →    "识别中…"    →   "识别文本：xxx"
 (on_record.py)      (pipeline.py)     (pipeline.py，6 秒后自己消失)
```

用的是 `my-custom-gnome-indicator` 的 CLI（GNOME Shell 扩展 + 本地 CLI，独立项目）。
三步是**同一条浮层**：都打到同一个 `--id`（`xiaomi-asr`）上，每次调用原地改写，
不是"关掉旧的再弹新的"——所以不会闪、不会堆叠、也不会在通知中心留下三条记录。

因此这里挂了**两个** hook（`install-service.sh install` 会自动写好）：

```json
{ "hooks": { "start": ["./on_record.py"], "end": ["./pipeline.py"] } }
```

两个 hook 是两个进程，彼此不传递任何状态——不需要，因为后端的 `update` 就是
"同 id 的 Notify"，id 不存在时直接创建。所以每一步都把完整参数发全，谁先落地谁负责
把浮层建出来，`start` 没跑到（服务刚重启、扩展刚装上）也不影响后面两步。
固定 id 而不按 PID 生成，是为了让上次跑崩遗留的浮层被下次直接顶掉。

### 谁的回合：迟到的调用必须作废

固定 id 有个反面：**一次迟到的调用会打到下一代的浮层上，甚至把已经消失的浮层唤回来。**
这不是假设——`start` hook 是 detached spawn，没人等它，它那一次 D-Bus 往返一旦卡住
（Shell 正忙、CLI 冷启动），就可能在 `end` 已经出完结果、浮层都过期消失之后才落地，
于是"正在录音…"在录音结束好几秒后凭空冒出来，然后挂满它 300s 的 ttl。

修法是排序，不是加锁：每个 hook 进程接手时把自己的 token 写进
`$XDG_STATE_HOME/xiaomi-remote-gateway/indicator.turn`（`claim()`），
每次调用后端**前后**各读一次这个文件（`_owns()`）：

- 调用前发现回合已经不是自己的 → 直接不画，日志记一行 `superseded by a newer run`。
- 调用期间被顶掉 → 屏幕上现在是自己那条陈旧的浮层（同 id，已经把新的覆盖掉了），
  于是无条件 `hide` 收回；新一代如果还有步骤要显示，它自己会再画一次。

最新的一代永远赢。

### 一步一张表

配置按**步骤**分表，不是按前缀铺平。每一步问的都是同样几个问题——要不要显示、怎么画、
最多停多久——答案就写在问题旁边：

```toml
[output.indicator]
enabled       = true
cli           = "my-custom-gnome-indicator-cli"
position      = "top-left"
indeterminate = "spinner"           # spinner|pulse
animate       = "2500ms"

[output.indicator.recording]        # 按下按键 → 正在录音…
show    = true
style   = "live"                    # minimal | live
ttl_sec = 300

[output.indicator.working]          # 松开按键 → 识别中… / 后处理中…
show  = true
style = "live"

[output.indicator.result]           # ASR 返回 → 识别文本：xxx
show               = true
duration_sec       = 2.5            # 成功时在屏幕上停多久
error_duration_sec = 8              # 失败 / 识别为空

[output.indicator.look]             # 颜色、圆角、内边距
bg = "slate-950/78"
border = "white/12"
text = "slate-300"
padding = "tight"
radius = "md"
```

（以前这些是 `[output]` 下一排 `indicator_*` 平铺键。旧键**不再被读取**，但也不会被
静默忽略：`load_config` 认得它们，每个旧键打一行日志告诉你现在该写在哪张表里。）

#### show：三步各自可开关

三步不是一个整体，想要哪几步就留哪几步。关掉一步不是"跳过这次调用"，是**在那个时间点把
浮层收掉**——否则上一步的浮层会留在屏幕上空等它的 ttl。所以 `working.show = false` 的
效果是：按下去出现"正在录音…"，松手即消失，不再有中间态。三个全 false 等价于
`enabled = false`。

`result.show = false` 时结果走上面的 `notify`（浮层和通知永远只出一个）；两个都不想要就
`notify = false` 一起关掉。

#### style：每一步的样式

| 样式 | 长什么样 |
| --- | --- |
| `minimal` | 一行字，零动效。进行中只由末尾的 `…` 表示（发的是 `info` 而不是 `progress`，连进度区域的位置都不占） |
| `live` | 转圈 + 慢速淡入淡出、颜色过渡 |

**第三步（结果）表里没有 `style`，也永远没有动画。** 它是终态：没有任何东西还在跑，
所以没有转圈可关；而在一行你正要读的字上做淡入淡出只是干扰。

**两种样式都不画图标、不画标题。** 这两样都是固定字符串，说不出任何你不知道的信息，
却各占一行高度、把正文往下推——而你每说一句话它就要弹一次，高度就是全部预算。
图标用 `--icon ''` 明确清掉，标题干脆不发。

### 时长：两种，别混

前两步的 `ttl_sec` **不是显示时长**，是兜底死人开关——正常情况下它们在自己那一步结束的
几毫秒内就被下一步改写掉了，没人会盯着它看满 `ttl_sec` 秒。它防的是**下一步根本没来**。

只有第三步是真的显示时长：它后面没有东西了，`duration_sec` 就是你实际看它多久。

| 步骤 | ttl | 什么时候真的会触发 |
| --- | --- | --- |
| 正在录音… | `[output.indicator.recording] ttl_sec`（默认 300s） | 按得太短没生成音频 → `end` 不触发；录音中途服务重启 |
| 识别中… | `[asr] timeout_sec` + 15s（`[output.indicator.working] ttl_sec` 可覆盖） | 进程被 SIGKILL，来不及写结果 |
| 后处理中… | `[llm] timeout_sec` + 15s | 同上 |
| 识别文本：xxx | `[output.indicator.result] duration_sec`（默认 2.5s） | 终态。文本这时已经在剪贴板里了，浮层只负责确认"到手了"，看一眼就够 |
| 失败 / 识别为空 | 同表 `error_duration_sec`（默认 8s） | 这个是真要读的 |

刻意不用一个全局上限：分步之后，浮层大约在"它自己说的那一步"放弃时消失，卡在哪一步一眼看得出。
另外能提前收掉的就提前收：录音短于 `min_duration_sec` 被丢弃时，`pipeline.py` 会直接
`hide` 掉"正在录音…"，不让它空转五分钟。

### 样式与动画：每一步只调一次

**这里没有任何轮询循环。** 三步就是三次 CLI 调用，动效全部由 Shell 自己插值：强调色渐变、
进出场淡入淡出，都是"发一个新值，剩下的交给合成器"。用定时器反复 `update`
去驱动动画既浪费（每帧一次进程创建 + 一次 D-Bus 往返），效果还不如合成器自己做的。
转圈那个 spinner 同理，是扩展内部在画（`St.SpinnerContent`，C 层）。

进行中**不画进度条**，画 spinner（`--indeterminate spinner`）。进度条要求你给出一个位置，
而这里根本没有诚实的位置可给：按住的时候不知道还要说多久，ASR 是一次不透明的往返。
卡在"不确定态"的进度条是在装样子，spinner 只说"在转"，刚好就是全部信息。
想换回来回跑的脉冲条就 `[output.indicator] indeterminate = "pulse"`；spinner 要 GNOME 49+，
低版本扩展自己退回脉冲条，不报错。

文案末尾的 **`…`** 是同一件事的文字版——`正在录音…` 是在进行，`识别文本：xxx` 是完成。

强调色（左侧竖条 + spinner）随步骤走，Shell 会在两色之间渐变，
所以看起来是同一个东西在变状态，而不是三个恰好长在同一位置的浮层：

| 步骤 | accent | 为什么 |
| --- | --- | --- |
| 正在录音… | `rose-400/80` | 麦克风在录，红色是所有录音指示灯的通用语言 |
| 识别中… / 后处理中… | `sky-400/70` | 在等网络 |
| 识别文本：xxx | `emerald-400/80` | 完成 |
| 失败 | 走 `error` 内置配色 | 本来就是对的 |

其余外观可配，默认值是往"安静"调的——这东西你每说一句话就要弹一次，
必须一眼看得清、其余时候能无视，否则很快就会变成你想关掉的东西。
所以底色、边框、正文都压暗，accent 是整个浮层唯一还算饱和的东西，而且也只有 4px 宽：

```toml
[output.indicator]
position      = "top-left"
indeterminate = "spinner"   # spinner|pulse，只对 live 样式有意义
animate       = "2500ms"    # 龟速，比 CLI 的 slow(400ms) 慢一个量级，见下

[output.indicator.look]
bg      = "slate-950/78"
border  = "white/12"
text    = "slate-300"
padding = "tight"           # none|tight|compact|normal|loose，或 "6px 10px"
radius  = "md"
```

`--animate` 收任意时长，不只是 `off|fast|normal|slow` 那四个词。默认就往**慢**调：
淡入淡出和颜色过渡拉到 2.5s，浮层不会"啪"地出现，而是你还在说话的时候慢慢渗上来、
状态之间慢慢滑过去——这是"被注意到"和"被打断"的区别。调慢对这边零成本：
帧全是 Shell 画的，2.5s 的渐变和 200ms 的渐变对脚本来说都是**同一次调用**。
（spinner 自己的转速属于扩展，`--animate` 管不着。）

> `--padding` 和 `--text` 是为这个用法给 indicator 新加的参数（`resolvePadding` 在
> `extension/palette.js`）。旧版扩展会忽略这两个键、不会报错，所以要真正生效得去
> indicator 仓库 `./install.sh` 再注销重登一次。

样式参数每次调用都发全，不靠"继承上一次"——步骤 1 没跑到时（服务刚重启），
步骤 2 建出来的浮层没有任何东西可继承。

### 没有扩展的时候

- 进度那两步**静默丢弃**，只有最终结果降级成 `notify-send`。
- 进度调用特意加了 `--no-fallback`：CLI 默认会在后端不可用时自己转 `notify-send`，
  照搬的话每次说话都多出一条谁也不会更新、谁也不会清理的"正在录音…"通知。
- 浮层和通知不会同时出现，同一件事只说一遍。

```toml
[output.indicator]
enabled = true
```

> `--padding`、`--text`、`--indeterminate spinner` 需要较新的 indicator；
> 旧版扩展忽略不认识的键、不会报错（spinner 会自动退回脉冲条）。

装法见该项目 README；`my-custom-gnome-indicator-cli doctor` 打出 `backend: ok` 才算生效，
Wayland 下装完要注销重登一次。

---

## 会话环境（为什么"终端里好用、当服务就没反应"）

`wl-copy` / `notify-send` / 浮层 CLI 都需要 `WAYLAND_DISPLAY` 和会话 D-Bus。用户级 unit 只有在登录时
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
| `[output.transcript] log` JSONL | 同表的 `max_bytes`（默认 1 MiB），超了就转成 `.jsonl.1`，只留一代 | `pipeline.py` |

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
