# Xiaomi Remote Pro 2 Minimal Server

**小米蓝牙遥控器2 Pro（RC003 / ARN9 普通款）** 接入使用的最小化最轻量级Binary Server。 仅用于Linux系统。

只做最核心一件事：**接收遥控器语音事件 → 保存临时音频文件 → 触发 Hook 脚本**。

无图形界面、无前端, 只完成以下**核心**流程:
```
(人)按下麦克风键说话
(Server) 开始记录音频
(人)松开后
(Server) 保存临时音频文件, 交给你的 Hook 脚本
(你的 Hook 脚本) 去调用ASR, LLM, Anything...
```


- 程序文件大小(binary size): `6.5MB`
- 实际物理内存（RSS）：`约 7.54 MB`

它不做:
1. 图形界面
2. 遥控器上除了语音按键以外的管理 (其他按键都是标准蓝牙键盘按键, 你可以使用任意专业软件进行按键映射管理, 详见[其他按键](docs/other-keys-handle.md))

<br>

Linux上直接就能用, 如果你使用Windows或Macos系统, 应该怎么使用这个项目:
- 把它当成一个template, clone到本地, 交给ai按照你的需求改造


---

[examples/](examples/) 包含几个使用示例，以及一个接入 Qwen ASR 3.1 的[真实使用示例](examples/4.daily-use/)。

安装、配置和使用详情见 [DETAILS.md](DETAILS.md)。
