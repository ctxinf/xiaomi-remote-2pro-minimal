# Xiaomi Remote Pro 2 Server Minimal

**小米蓝牙语音遥控器（RC003 / 小米蓝牙遥控器 v2 Pro / ARN9 普通款）** 接入的最小化轻量级Server。

只做一件事：**接收遥控器语音事件 → 保存临时音频文件 → 触发 Hook 脚本**。

无图形界面、无前端, 只完成以下**核心**流程:
```
(人)按下麦克风键说话
(Server) 开始记录音频
(人)松开后
(Server) 保存临时音频文件, 交给你的 Hook 脚本
(你的 Hook 脚本) 去调用ASR, LLM, Anything...
```



它不做:
1. 遥控器上除了语音按键以外的管理 (其他按键都是标准蓝牙键盘按键, 你可以使用任意专业软件进行按键映射管理, 详见[其他按键](docs/other-keys-handle.md))
2. 音频流 stream 输出

你应该怎么使用这个项目:
把它当成一个template, clone到本地, 交给ai按照你的需求改造


---

## 技术说明

- 完整实现 Google Android TV Voice-over-BLE (ATVV) 私有 GATT 协议与协商流程；
- 内置 IMA/DVI ADPCM 解码器（16 kHz / 16-bit 线性 PCM）；
- 3 抽头音频平滑滤波与可调数字增益；
- 自动发现已连接/广播的遥控器，断线自动重连；
- 通过 evdev 独占 + uinput 转发拦截麦克风键，其余按键透明直通，并在 keyd 等重映射器在场时自动让位；
- 启动时打印遥控器型号、固件版本与电池电量。



## 快速开始

```bash
cargo build --release
cp config.example.jsonc config.jsonc
./target/release/xiaomi-remote-pro2-gateway --config ./config.jsonc
```

先在系统蓝牙设置中配对遥控器。编辑 `hooks.end` 为你自己的可执行脚本；ASR、LLM、API 密钥和输出逻辑都在脚本中配置，Rust 只负责语音输入。

- [配置、命令行与日志](docs/cli.md)
- [Hook 接口与示例](docs/hooks.md)
- [录音流程](docs/workflow.md) · [自动清理](docs/auto-clean.md)
- [可选 systemd + ASR/LLM 示例](examples/real-use-systemd/README.md)
