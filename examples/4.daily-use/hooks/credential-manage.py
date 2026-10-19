#!/usr/bin/env python3
"""交互式管理 hooks-config.jsonc 的 api_key；不输出凭据明文。"""
import argparse
import getpass
import json
import os
from pathlib import Path
import re
import sys
import tempfile

from hooks_common import CONFIG, HERE, Fail, SecretStore, read_config


def write_reference(reference="${secret:api_key}"):
    text = CONFIG.read_text()
    # 只改 api_key 的值，保留其他配置和注释。
    pattern = r'("(?:\\.|[^"\\])*"|//[^\n]*|/\*.*?\*/)|(\s+)'
    # 使用 JSON token 定位字段，注释中的 api_key 不参与匹配。
    tokens = list(re.finditer(pattern + r'|([{}:,\[\]])|([^\s{}:,\[\]]+)', text, re.S))
    meaningful = [m for m in tokens if not m[0].isspace() and not m[0].startswith(('//', '/*'))]
    for index, token in enumerate(meaningful[:-2]):
        if token[0] == '"api_key"' and meaningful[index + 1][0] == ':':
            value = meaningful[index + 2]
            text = text[:value.start()] + json.dumps(reference) + text[value.end():]
            break
    else:
        raise Fail("配置缺少 api_key 字段，请从示例配置初始化")
    fd, name = tempfile.mkstemp(dir=HERE, prefix='.hooks-config-')
    try:
        with os.fdopen(fd, 'w') as file:
            file.write(text)
        Path(name).replace(CONFIG)
    finally:
        Path(name).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['init', 'set', 'encrypt', 'status', 'delete'])
    args = parser.parse_args()
    if not CONFIG.exists():
        fd = os.open(CONFIG, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as file:
            file.write((HERE / 'hooks-example.config.jsonc').read_text())
    if args.command == 'init':
        print(f'Hook 配置：{CONFIG}')
        return
    store = SecretStore()
    if args.command in ('set', 'encrypt'):
        value = getpass.getpass('API Key（隐藏输入）：') if args.command == 'set' else read_config()['api_key']
        if not value or value.startswith('${secret:'):
            raise Fail('没有可加密的 API Key')
        store.set('api_key', value)
        write_reference()
        print('API Key 已加密保存，配置中仅保留凭据引用')
    elif args.command == 'delete':
        write_reference('')
        store.remove('api_key')
        print('已移除 API Key')
    else:
        configured = read_config()['api_key']
        if configured == '${secret:api_key}':
            state = '已加密' if 'api_key' in store.names() else '凭据缺失'
        else:
            state = '明文，运行 encrypt 可加密' if configured else '未配置'
        print(f'API Key：{state}')
    print(f'配置：{CONFIG}\n凭据：{store.path}\n本地密钥：{store.key_path}')


if __name__ == '__main__':
    try:
        main()
    except (Fail, OSError, ValueError):
        print('凭据操作失败，请检查配置和 openssl；未输出凭据内容', file=sys.stderr)
        sys.exit(1)
