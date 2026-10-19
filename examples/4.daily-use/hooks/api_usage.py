"""API 用量 JSONL：只记录计量数据，不记录 API Key、音频或识别文本。"""
from collections import defaultdict
from datetime import datetime, timezone
import fcntl
import json
import math
import os
from pathlib import Path
import sys
import wave

DEFAULT_LOG = Path(__file__).resolve().parent / 'api-usage.jsonl'


def audio_seconds(audio):
    try:
        with wave.open(str(audio), 'rb') as recording:
            return recording.getnframes() / recording.getframerate()
    except (OSError, EOFError, wave.Error):
        return None


def record_call(audio, model, elapsed, result=None, error=None, path=None):
    result = result if isinstance(result, dict) else {}
    row = {
        'timestamp': datetime.now(timezone.utc).isoformat(),
        'model': model,
        'status': 'error' if error is not None else 'ok',
        'audio_seconds': audio_seconds(audio),
        'request_seconds': round(elapsed, 6),
        'request_id': result.get('request_id'),
        'usage': result.get('usage'),
    }
    if error is not None:
        row['error_type'] = type(error).__name__
        row['http_status'] = getattr(error, 'code', None)
    try:
        # 多个 end hook 可以并发；锁住整行写入，文件创建时即为 0600。
        fd = os.open(path or DEFAULT_LOG, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        with os.fdopen(fd, 'a') as file:
            fcntl.flock(file, fcntl.LOCK_EX)
            file.write(json.dumps(row, ensure_ascii=False) + '\n')
            file.flush()
    except (OSError, TypeError, ValueError):
        print('[hook:usage] 统计记录写入失败，不影响识别和复制', file=sys.stderr)


def numeric_fields(value, prefix='usage'):
    if isinstance(value, dict):
        for key, item in value.items():
            yield from numeric_fields(item, f'{prefix}.{key}')
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from numeric_fields(item, f'{prefix}[{index}]')
    elif isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value):
        yield prefix, value


def summarize(path):
    count = errors = invalid = 0
    duration = elapsed = 0.0
    durations_reported = 0
    totals = defaultdict(float)
    reported = defaultdict(int)
    if not path.exists():
        print(f'暂无 API 调用记录：{path}')
        return
    with path.open() as file:
        for line in file:
            if not line.strip():
                continue
            try:
                row = json.loads(line)
                if not isinstance(row, dict):
                    raise ValueError('invalid record')
            except (ValueError, TypeError):
                invalid += 1
                continue
            count += 1
            errors += row.get('status') == 'error'
            if isinstance(row.get('audio_seconds'), (int, float)):
                duration += row['audio_seconds']
                durations_reported += 1
            elapsed += row.get('request_seconds') or 0
            for key, value in numeric_fields(row.get('usage')):
                totals[key] += value
                reported[key] += 1
    print(f'记录文件：{path}')
    print(f'API 调用：{count} 次，成功响应 {count - errors} 次，失败 {errors} 次')
    print(f'提交音频累计：{duration:.3f} 秒（{durations_reported} 次可计量；重试重复计入）')
    print(f'请求耗时累计：{elapsed:.3f} 秒')
    if not totals:
        print('暂无 API 返回的 usage 计量数据')
    for key, value in sorted(totals.items()):
        print(f'{key}: {value:g}（{reported[key]} 次有返回）')
    if invalid:
        print(f'跳过 {invalid} 行无效记录', file=sys.stderr)
