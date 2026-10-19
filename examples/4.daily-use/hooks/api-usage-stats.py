#!/usr/bin/env python3
"""汇总 API 用量：音频秒数、请求耗时及 API 返回的 input/output/cache 等计量。"""
import argparse
from pathlib import Path
import sys

from api_usage import DEFAULT_LOG, summarize

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--file', type=Path, default=DEFAULT_LOG, help='JSONL 记录文件')
    args = parser.parse_args()
    try:
        summarize(args.file)
    except KeyboardInterrupt:
        sys.exit(130)
    except OSError as error:
        print(f'无法读取统计记录：{error.strerror}', file=sys.stderr)
        sys.exit(1)
