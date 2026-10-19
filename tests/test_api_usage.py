"""用量统计只使用临时文件和模拟 API 返回数据。"""
from concurrent.futures import ThreadPoolExecutor
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
import wave
from urllib.error import HTTPError

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'examples/4.daily-use/hooks'))
from api_usage import record_call, summarize


class ApiUsageTests(unittest.TestCase):
    def test_concurrent_append_and_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            audio = Path(directory) / 'sample.wav'
            with wave.open(str(audio), 'wb') as file:
                file.setnchannels(1)
                file.setsampwidth(2)
                file.setframerate(16000)
                file.writeframes(b'\0' * 32000)
            log = Path(directory) / 'usage.jsonl'
            result = {
                'output': {'text': 'private-transcript-placeholder'},
                'request_id': 'test-id',
                'usage': {'input_tokens': 10, 'output_tokens': 2,
                          'input_tokens_details': {'cached_tokens': 4}, 'seconds': 1.0},
            }
            with ThreadPoolExecutor(max_workers=4) as pool:
                list(pool.map(lambda _: record_call(audio, 'test-model', 0.25, result=result, path=log), range(12)))
            with HTTPError('https://test.invalid', 429, 'test', {}, io.BytesIO()) as error:
                record_call(audio, 'test-model', 0.5, error=error, path=log)
            lines = log.read_text().splitlines()
            self.assertEqual(len(lines), 13)
            self.assertNotIn('private-transcript-placeholder', log.read_text())
            self.assertEqual(json.loads(lines[-1])['http_status'], 429)
            self.assertEqual(json.loads(lines[0])['usage'], result['usage'])
            self.assertEqual(log.stat().st_mode & 0o777, 0o600)
            with log.open('a') as file:
                file.write('{truncated\n')
            output, errors = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                summarize(log)
            summary = output.getvalue()
            self.assertIn('13 次，成功响应 12 次，失败 1 次', summary)
            self.assertIn('13.000 秒', summary)
            self.assertIn('3.500 秒', summary)
            self.assertIn('usage.input_tokens: 120', summary)
            self.assertIn('usage.output_tokens: 24', summary)
            self.assertIn('usage.input_tokens_details.cached_tokens: 48', summary)
            self.assertIn('usage.seconds: 12', summary)
            self.assertIn('跳过 1 行', errors.getvalue())

    def test_missing_usage_and_unwritable_log(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'usage.jsonl'
            record_call(Path(directory) / 'missing.wav', 'test-model', 0.1, result={}, path=path)
            with contextlib.redirect_stdout(io.StringIO()) as output:
                summarize(path)
            self.assertIn('暂无 API 返回的 usage', output.getvalue())
            # 统计失败不阻断调用者。
            with contextlib.redirect_stderr(io.StringIO()) as errors:
                record_call(Path(directory) / 'missing.wav', 'test-model', 0.1,
                            path=Path(directory) / 'missing-directory/usage.jsonl')
            self.assertIn('不影响识别和复制', errors.getvalue())


if __name__ == '__main__':
    unittest.main()
