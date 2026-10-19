# systemd + ASR/LLM Hook 示例

这是可选的 Python Hook 部署示例。Rust 网关只处理蓝牙语音、WAV 文件和 Hook 事件。

```bash
./install-service.sh config init
./install-service.sh secret add dashscope_api_key YOUR_KEY
./install-service.sh secret add llm_api_key YOUR_KEY
./install-service.sh
```

需要 Python 3.11+、OpenSSL、systemd 用户服务；剪贴板输出需要 `wl-copy` 或 `xclip`。

配置位于 `~/.config/xiaomi-remote-gateway/`：

- `config.jsonc`：安装器和网关设置，默认自动寻找 release 二进制。修改后重新运行安装器。
- `hook.jsonc`：ASR、LLM、剪贴板等脚本设置，每次录音重新读取。关闭 `llm.enabled` 可直接输出识别结果。

两个文件都支持 `//`、`/* */` 注释，不支持尾逗号。省略的选项使用脚本内置默认值。
API 密钥使用 `${secret:name}` 引用，通过 `secret add` 管理。加密存储使用同用户本地密钥。
旧 TOML 不再读取；请先运行 `config init`，再手动迁移需要的设置。

```bash
./install-service.sh status
./install-service.sh logs
./install-service.sh doctor
./install-service.sh restart
./install-service.sh uninstall
```

安装器配置与原生网关 `--config` 配置格式不同：前者包含 `gateway` 分组，后者使用仓库根目录的 [config.example.jsonc](../../config.example.jsonc)。原生网关完全不读取 `hook.jsonc`。
