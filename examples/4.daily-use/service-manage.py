#!/usr/bin/env python3
"""安装/更新用户级 systemd 服务，以及查看状态、日志和安装位置。"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
APP = 'xiaomi-remote-gateway'
DATA = Path(os.environ.get('XDG_DATA_HOME') or Path.home() / '.local/share') / APP
UNIT = Path(os.environ.get('XDG_CONFIG_HOME') or Path.home() / '.config') / 'systemd/user' / f'{APP}.service'


def systemctl(*args, check=True):
    return subprocess.run(['systemctl', '--user', *args], check=check)


def locations():
    print(f'Service：{UNIT}\n程序：{DATA / "bin/xiaomi-remote-2pro-minimal"}\n'
          f'网关配置：{DATA / "config.jsonc"}\nHooks 程序：{DATA / "hooks"}\n'
          f'Hook 配置：{DATA / "hooks/hooks-config.jsonc"}\n'
          f'凭据管理：{DATA / "hooks/credential-manage.py"}')


def copy_file(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    # 原子替换运行中的二进制，避免 Text file busy。
    fd, name = tempfile.mkstemp(dir=destination.parent)
    os.close(fd)
    try:
        shutil.copy2(source, name)
        Path(name).replace(destination)
    finally:
        Path(name).unlink(missing_ok=True)


def quote(path):
    # systemd 的 ExecStart 使用自己的引号规则；转义 % 和 $ specifier。
    return '"' + str(path).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%').replace('$', '$$') + '"'


def install(build):
    if build:
        subprocess.run(['cargo', 'build', '--release', '--manifest-path', str(ROOT / 'Cargo.toml')], check=True)
    binary = ROOT / 'target/release/xiaomi-remote-2pro-minimal'
    if not binary.is_file():
        raise ValueError('找不到 release 二进制，请先 cargo build --release')
    copy_file(binary, DATA / 'bin' / binary.name)
    # 用户配置和凭据始终保留；更新仅覆盖程序和示例配置。
    if not (DATA / 'config.jsonc').exists():
        copy_file(HERE / 'config.jsonc', DATA / 'config.jsonc')
    for source in (HERE / 'hooks').iterdir():
        if source.suffix == '.py' or source.name == 'hooks-example.config.jsonc':
            copy_file(source, DATA / 'hooks' / source.name)
    subprocess.run([sys.executable, str(DATA / 'hooks/credential-manage.py'), 'init'], check=True)
    template = (HERE / 'xiaomi-remote-gateway.service.template').read_text()
    unit_text = template.replace('@BINARY@', quote(DATA / 'bin' / binary.name)).replace('@CONFIG@', quote(DATA / 'config.jsonc'))
    UNIT.parent.mkdir(parents=True, exist_ok=True)
    UNIT.write_text(unit_text)
    session_vars = [name for name in ('WAYLAND_DISPLAY', 'DISPLAY', 'XAUTHORITY', 'XDG_CURRENT_DESKTOP') if os.environ.get(name)]
    if session_vars:
        systemctl('import-environment', *session_vars)
    systemctl('daemon-reload')
    systemctl('enable', f'{APP}.service')
    systemctl('restart', f'{APP}.service')
    print('已安装/更新为用户级 service')
    locations()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', nargs='?', default='status',
                        choices=['install', 'update', 'status', 'paths', 'logs', 'restart', 'uninstall'])
    parser.add_argument('--no-build', action='store_true', help='复用 target/release 中的二进制')
    args = parser.parse_args()
    if args.command in ('install', 'update'):
        install(not args.no_build)
    elif args.command == 'status':
        print('已安装为 service' if UNIT.exists() else '尚未安装为 service')
        locations()
        if UNIT.exists():
            systemctl('status', f'{APP}.service', check=False)
    elif args.command == 'paths':
        locations()
    elif args.command == 'logs':
        subprocess.run(['journalctl', '--user', '-u', f'{APP}.service', '-f'], check=True)
    elif args.command == 'restart':
        systemctl('restart', f'{APP}.service')
    else:
        systemctl('disable', '--now', f'{APP}.service')
        UNIT.unlink(missing_ok=True)
        systemctl('daemon-reload')
        print('已卸载 service；程序、配置与凭据保留在：')
        locations()


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        print('\n已停止当前操作。', file=sys.stderr)
        sys.exit(130)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'服务管理失败：{error}', file=sys.stderr)
        sys.exit(1)
