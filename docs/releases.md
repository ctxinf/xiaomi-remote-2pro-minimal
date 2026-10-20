# 自动发布

推送新 tag 后，GitHub Actions 会运行 Rust 和 Python 测试，构建 Linux x86_64 与 ARM64 二进制，并创建 GitHub Release。建议使用 `v` 开头的版本号。

```sh
git tag v0.1.0
git push origin v0.1.0
```

发布前请同步 `Cargo.toml` 和 `Cargo.lock` 中的版本号。已有 Release 不会被工作流覆盖。

每个架构提供一个 `.tar.gz`，包含可执行文件、示例配置和使用文档。`SHA256SUMS` 用于校验下载文件：

```sh
sha256sum --ignore-missing -c SHA256SUMS
```

x86_64 在 Ubuntu 22.04 构建（glibc 2.35+）；ARM64 在 Ubuntu 24.04 构建（glibc 2.39+）。运行时需要 BlueZ 和系统 D-Bus 库；这些构建不是静态二进制。Debian / Ubuntu 可安装 `bluez` 和 `libdbus-1-3`，其余配置参见 [使用详情](../DETAILS.md)。
