"""Daily-use 示例测试：只使用临时目录、假凭据及模拟 API/systemd。"""
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

EXAMPLE = Path(__file__).resolve().parents[1] / 'examples/4.daily-use'
sys.path.insert(0, str(EXAMPLE / 'hooks'))
import hooks_common as common


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


entry = load('daily_end', EXAMPLE / 'hooks/hook-end-entry.py')
credentials = load('daily_credentials', EXAMPLE / 'hooks/credential-manage.py')
service = load('daily_service', EXAMPLE / 'service-manage.py')


class DailyUseTests(unittest.TestCase):
    def setUp(self):
        # API 模拟测试不向真实示例目录写统计。
        self.usage_patch = patch.object(entry, 'record_call')
        self.record_call = self.usage_patch.start()
        self.addCleanup(self.usage_patch.stop)

    def test_jsonc_and_reference_preserve_comments(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / 'hooks-config.jsonc'
            config.write_text('// "api_key": "comment"\n{\n"endpoint":"https://test/api", /* note */\n"api_key":"test-placeholder"\n}')
            self.assertEqual(common.read_config(config)['endpoint'], 'https://test/api')
            with patch.object(credentials, 'CONFIG', config), patch.object(credentials, 'HERE', config.parent):
                credentials.write_reference()
            self.assertIn('// "api_key": "comment"', config.read_text())
            self.assertEqual(common.read_config(config)['api_key'], '${secret:api_key}')
            self.assertEqual(config.stat().st_mode & 0o777, 0o600)
            self.assertEqual(common.read_config(EXAMPLE / 'hooks/hooks-example.config.jsonc')['model'], 'qwen-audio-3.1-asr-flash')

    @unittest.skipUnless(shutil.which('openssl'), 'OpenSSL required')
    def test_encrypted_credentials_roundtrip(self):
        with tempfile.TemporaryDirectory() as directory:
            store = common.SecretStore(Path(directory) / 'credentials.enc', Path(directory) / 'credentials.key')
            store.set('api_key', 'test-placeholder')
            self.assertNotIn(b'test-placeholder', store.path.read_bytes())
            self.assertEqual(store.load(), {'api_key': 'test-placeholder'})
            for path in (store.path, store.key_path):
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            store.key_path.unlink()
            with self.assertRaises(common.Fail):
                store.load()
            self.assertFalse(store.key_path.exists())

    def test_native_request_and_response(self):
        with tempfile.TemporaryDirectory() as directory:
            audio = Path(directory) / 'test.wav'
            audio.write_bytes(b'RIFF-test')
            def respond(request, timeout):
                body = json.loads(request.data)
                self.assertEqual(body['model'], 'qwen-audio-3.1-asr-flash')
                self.assertEqual(body['parameters'], {'format': 'wav', 'sample_rate': '16000'})
                self.assertTrue(body['input']['messages'][0]['content'][0]['input_audio']['data'].startswith('data:audio/wav;base64,'))
                return io.BytesIO(json.dumps({'output': {'choices': [{'message': {'content': [{'text': '你好'}]}}]}}).encode())
            with patch.object(entry, 'urlopen', side_effect=respond):
                self.assertEqual(entry.transcribe(audio, {**common.DEFAULTS, 'api_key': 'test-placeholder'}), '你好')

    def test_native_text_response_and_invalid_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            audio = Path(directory) / 'test.wav'
            audio.write_bytes(b'RIFF-test')
            for output, expected in (({'text': '  你好  ', 'sentence': []}, '你好'),
                                     ({'text': ''}, '')):
                response = io.BytesIO(json.dumps({'output': output}).encode())
                with patch.object(entry, 'urlopen', return_value=response):
                    self.assertEqual(entry.transcribe(audio, {**common.DEFAULTS, 'api_key': 'test-placeholder'}), expected)
            with patch.object(entry, 'urlopen', return_value=io.BytesIO(b'{"output":{}}')):
                with self.assertRaisesRegex(common.Fail, 'output.text'):
                    entry.transcribe(audio, {**common.DEFAULTS, 'api_key': 'test-placeholder'})
            # 解析失败前已记录收到的响应，以免漏计 API 用量。
            self.assertEqual(self.record_call.call_args.kwargs['result'], {'output': {}})
            with patch.object(entry, 'urlopen', side_effect=OSError('network-test')):
                with self.assertRaises(OSError):
                    entry.transcribe(audio, {**common.DEFAULTS, 'api_key': 'test-placeholder'})
            self.assertIsInstance(self.record_call.call_args.kwargs['error'], OSError)

    def test_recording_retained_on_clipboard_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            audio = Path(directory) / 'test.wav'
            audio.write_bytes(b'RIFF-test')
            with patch.dict(os.environ, XIAOMI_AUDIO_FILE=str(audio)), patch.object(entry, 'read_config', return_value={}), patch.object(entry, 'transcribe', return_value='text'), patch.object(entry, 'copy_text', side_effect=common.Fail('test')):
                with self.assertRaises(common.Fail):
                    entry.main()
                self.assertTrue(audio.exists())
            with patch.dict(os.environ, XIAOMI_AUDIO_FILE=str(audio)), patch.object(entry, 'read_config', return_value={}), patch.object(entry, 'transcribe', return_value='text'), patch.object(entry, 'copy_text'):
                entry.main()
                self.assertFalse(audio.exists())

    def test_logs_ctrl_c_exits_without_traceback(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / 'journalctl'
            journal.write_text('#!/bin/sh\necho READY\nexec sleep 30\n')
            journal.chmod(0o755)
            process = subprocess.Popen(
                [sys.executable, str(EXAMPLE / 'service-manage.py'), 'logs'],
                env={**os.environ, 'PATH': directory + os.pathsep + os.environ.get('PATH', '')},
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                start_new_session=True,
            )
            try:
                self.assertEqual(process.stdout.readline().strip(), 'READY')
                os.killpg(process.pid, signal.SIGINT)
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130)
                self.assertNotIn('Traceback', stdout + stderr)
                self.assertIn('已停止当前操作', stderr)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.communicate()

    def test_install_update_preserves_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'target/release/xiaomi-remote-2pro-minimal'
            binary.parent.mkdir(parents=True)
            binary.write_text('fake binary')
            data = root / 'installed data'
            unit = root / 'systemd/test.service'
            def run(command, **kwargs):
                # 模拟 credential init，不触碰真实安装或桌面会话。
                if command[-1] == 'init':
                    config = data / 'hooks/hooks-config.jsonc'
                    if not config.exists():
                        config.write_text('{"api_key":""}')
            with patch.object(service, 'ROOT', root), patch.object(service, 'DATA', data), patch.object(service, 'UNIT', unit), patch.object(service, 'systemctl') as ctl, patch.object(service.subprocess, 'run', side_effect=run):
                service.install(False)
                config = data / 'hooks/hooks-config.jsonc'
                config.write_text('{"api_key":"user-placeholder"}')
                (data / 'config.jsonc').write_text('{"voice_key":"never"}')
                service.install(False)
                self.assertIn('user-placeholder', config.read_text())
                self.assertEqual((data / 'config.jsonc').read_text(), '{"voice_key":"never"}')
                self.assertIn('ExecStart="', unit.read_text())
                self.assertTrue((data / 'hooks/hook-end-entry.py').is_file())
                self.assertFalse((data / 'keyd-example').exists())
                ctl.assert_any_call('restart', 'xiaomi-remote-gateway.service')


if __name__ == '__main__':
    unittest.main()
