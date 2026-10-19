# 配置与命令行

最小配置见 [config.example.jsonc](../config.example.jsonc)：只需指定 `hooks.end`。运行：

```bash
./target/release/xiaomi-remote-pro2-gateway --config ./config.jsonc
```

JSONC 支持 `//` 和 `/* */` 注释，不支持尾逗号。不会自动读取配置文件。
相对脚本路径和 `record_dir` 以配置文件目录为基准；CLI 参数优先于配置。
网关配置不接受 ASR、LLM 或 API 密钥字段。

可选字段（通常不必填写）：

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `record_dir` | 系统临时目录下 `xiaomi-remote-gateway` | WAV 目录 |
| `gain` | `0` | 数字增益，-24～24 dB |
| `device_name` | 自动发现 | 蓝牙设备名过滤 |
| `voice_key` | `auto` | `auto` / `always` / `never` |
| `reconnect` | `3` | 初始重连间隔，秒，最小 1 秒 |
| `keep_hours` | `24` | 保留小时数，0 不限 |
| `keep_max` | `100` | 最多保留数量，0 不限 |
| `hooks` | 无 | `start` / `end` 到脚本或脚本数组的映射 |

上述字段也有同名 CLI 参数，下划线改为连字符，如 `--record-dir`。
临时使用也可直接传 `--hook ./my_hook.sh`，无需配置文件。
旧 `--hook-config` 仍可用，并支持 JSONC；JSON 文件仍然兼容。
同时提供多个来源时，按 `--hook-config`、`--config`、`--hook` 顺序注册，各脚本独立并发执行，不保证完成顺序。

```bash
./target/release/xiaomi-remote-pro2-gateway scan --seconds 5
./target/release/xiaomi-remote-pro2-gateway --config ./config.jsonc --debug
```

日志分级：

- `ERROR`：录音写入、协议或 Hook 启动失败。
- `WARN`：断线重连、Hook 非零退出和脚本 stderr。
- `INFO`：正常连接、录音文件、音频统计和脚本 stdout。
- `DEBUG`：控制收发、同步恢复、Hook PID、耗时和退出结果。
- `TRACE`：逐包音频长度、解码数量、丢弃原因。

网关默认输出 INFO 及以上，底层依赖保留 WARN 及以上；`--debug` 一次开启全部级别，包括 TRACE，`-v` / `--verbose` 为兼容别名。
日志含毫秒时间戳、级别和模块。Hook 输出会原样进入日志，请自行避免打印密钥和不希望记录的识别文本。
排查时保存从启动、按键录音到松开后的完整日志。

## 将诊断日志写入文件

```bash
./target/release/xiaomi-remote-pro2-gateway --config ./config.example.jsonc --debug --log-file ./gateway-debug.log
```

日志同时输出到终端 stderr 和指定文件；文件不存在时创建，存在时追加，父目录需已存在。
按 Ctrl-C 结束后，可直接让代码助手读取仓库中的 `gateway-debug.log` 排查。
文件日志不含终端颜色编码。不自动轮转，诊断完成后可自行删除。
扫描命令的设备列表仍输出到 stdout，不属于日志。
