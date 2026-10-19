从仓库根目录运行，需要 Python 3、OpenSSL，以及 `wl-clipboard`（Wayland）或 `xclip`（X11）。

```sh
# 本地初始化 hooks 配置并隐藏输入 API Key
python3 examples/4.daily-use/hooks/credential-manage.py init
python3 examples/4.daily-use/hooks/credential-manage.py set
./target/release/xiaomi-remote-pro2-gateway --config examples/4.daily-use/config.jsonc

# 安装为用户级 systemd service（自动构建 release）
examples/4.daily-use/install-service.sh install
examples/4.daily-use/install-service.sh status
examples/4.daily-use/install-service.sh paths
```

安装后的 hooks 使用独立配置。按 `paths` 显示的位置运行已安装的 `credential-manage.py set`，设置 service 使用的 API Key；仓库内的凭据不会复制到安装目录。

`install-service.sh update` 更新二进制、hooks 程序及 service 模板并重启，保留已安装的 `config.jsonc`、`hooks/hooks-config.jsonc` 与凭据。可添加 `--no-build` 复用 release 二进制。另有 `logs`、`restart`、`uninstall`；卸载 service 保留程序与配置。

- `config.jsonc`：网关配置，直接传给 binary；hook 路径相对该文件。
- `hooks/hook-start-entry-do-nothing.py`：start 入口，仅打印日志。
- `hooks/hook-end-entry.py`：end 入口，调用 ASR 后写入剪贴板；成功后删除录音，失败时保留。
- `hooks/hooks-example.config.jsonc`：hooks 的端点、模型与 API Key 配置示例；初始化为同目录的 `hooks-config.jsonc`。默认使用 DashScope 原生多模态接口和 `qwen-audio-3.1-asr-flash`。
- `hooks/credential-manage.py`：`set` 隐藏输入并加密，`encrypt` 加密配置里已有的明文 API Key，`status` 查看配置状态，`delete` 移除凭据。配置中保留 `${secret:api_key}` 引用；密文与本地密钥分别保存在 `credentials.enc`、`credentials.key`。二者均使用 0600 权限；能读取二者的本机用户仍可解密。
- `xiaomi-remote-gateway.service.template`：独立的 systemd 模板。
- `keyd-example/`：仅供参考，自行修改并应用，安装脚本不会处理。

桌面会话变更后，可在当前图形终端重新运行安装或更新命令，将显示会话变量同步给用户级 systemd。

API 调用会追加到 hooks 同目录的 `api-usage.jsonl`（权限 0600）。记录 UTC 时间、模型、请求状态、音频时长、请求耗时，以及响应原样返回的 `usage` 和 `request_id`；不保存音频、识别文字或 API Key。统计写入失败不影响复制。

```sh
python3 examples/4.daily-use/hooks/api-usage-stats.py
# 查看已安装版本的统计
python3 ~/.local/share/xiaomi-remote-gateway/hooks/api-usage-stats.py
```

汇总逐项累加 API 返回的所有数值计量字段，包括嵌套的 input/output/cache token、API 音频秒数等，保留原字段路径。API 未返回的字段不会推算。总 token 与其明细分别展示，不重复相加。提交音频累计秒数由 WAV 时长计算，包括失败调用和重试；请求耗时单独统计。记录收到成功响应时即写入，后续文本解析或复制失败不影响 API 用量记录。历史调用没有记录，统计从启用后开始。
