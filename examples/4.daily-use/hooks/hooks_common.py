"""Hook 配置与本地凭据存储；不依赖网关或 systemd。"""
import json
import os
from pathlib import Path
import re
import secrets as _secrets
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
CONFIG = HERE / "hooks-config.jsonc"
DEFAULTS = {
    "endpoint": "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation",
    "model": "qwen-audio-3.1-asr-flash",
    "api_key": "",
}

class Fail(Exception):
    pass

def log(message):
    print(message, flush=True)

def read_config(path=CONFIG):
    # 匹配完整 JSON 字符串，避免把 https:// 中的 // 当作注释。
    pattern = r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*.*?\*/'
    text = re.sub(pattern, lambda m: m[0] if m[0].startswith('"') else " ",
                  path.read_text(), flags=re.S)
    return {**DEFAULTS, **json.loads(text)}

def api_key(config):
    value = config["api_key"]
    if value == "${secret:api_key}":
        value = SecretStore().load().get("api_key", "")
    if not value:
        raise Fail("请运行 hooks/credential-manage.py set 配置 API Key")
    return value

class SecretStore:
    """凭据使用 OpenSSL AES-256-CBC / PBKDF2 加密，本地密钥权限为 0600。

    凭据和密钥都在本机；用于避免明文展示，不能防御能读取二者的用户。
    """

    def __init__(self, path: Path = None, key_path: Path = None):
        self.path = path or HERE / "credentials.enc"
        self.key_path = key_path or HERE / "credentials.key"

    # -- key ---------------------------------------------------------------

    def _openssl(self) -> str:
        exe = shutil.which("openssl")
        if not exe:
            raise Fail("openssl not found; install it (dnf install openssl) to store secrets")
        return exe

    def _ensure_key(self) -> str:
        if not self.key_path.exists():
            self.key_path.parent.mkdir(parents=True, exist_ok=True)
            # Written 0600 from the start: creating it world-readable and
            # chmod'ing after would leave a window where the key is exposed.
            fd = os.open(self.key_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w") as fh:
                fh.write(_secrets.token_hex(32))
            log(f"[secret] generated new key {self.key_path}")
        mode = self.key_path.stat().st_mode & 0o777
        if mode & 0o077:
            # A key another user can read is not a key.
            self.key_path.chmod(0o600)
            log(f"[secret] tightened permissions on {self.key_path} ({mode:o} -> 600)")
        return self.key_path.read_text().strip()

    # -- crypto ------------------------------------------------------------

    def _run_openssl(self, args: list[str], data: bytes, passphrase: str) -> bytes:
        # The passphrase goes over a dedicated pipe fd, never argv (which is
        # world-readable in /proc) and never an env var.
        r, w = os.pipe()
        try:
            os.write(w, passphrase.encode())
            os.close(w)
            w = -1
            proc = subprocess.run(
                [self._openssl(), "enc", *args, "-pbkdf2", "-iter", "600000",
                 "-pass", f"fd:{r}"],
                input=data,
                capture_output=True,
                pass_fds=(r,),
            )
        finally:
            if w != -1:
                os.close(w)
            os.close(r)
        if proc.returncode != 0:
            raise Fail(f"openssl failed: {proc.stderr.decode(errors='replace').strip()}")
        return proc.stdout

    def load(self) -> dict[str, str]:
        if not self.path.exists():
            return {}
        if not self.key_path.exists():
            raise Fail("credentials.key 缺失，无法解密已有凭据")
        raw = self._run_openssl(["-d", "-aes-256-cbc", "-base64", "-A"],
                                self.path.read_bytes(), self._ensure_key())
        try:
            return json.loads(raw or b"{}")
        except json.JSONDecodeError as e:
            raise Fail(f"{self.path} did not decrypt to valid JSON ({e}); "
                       f"was credentials.key replaced?") from e

    def save(self, values: dict[str, str]) -> None:
        blob = json.dumps(values, ensure_ascii=False, indent=2).encode()
        out = self._run_openssl(["-e", "-aes-256-cbc", "-base64", "-A"],
                                blob, self._ensure_key())
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, name = tempfile.mkstemp(dir=self.path.parent, prefix=".credentials-")
        try:
            with os.fdopen(fd, "wb") as file:
                file.write(out)
            Path(name).replace(self.path)
        finally:
            Path(name).unlink(missing_ok=True)

    def set(self, name: str, value: str) -> None:
        values = self.load()
        values[name] = value
        self.save(values)

    def remove(self, name: str) -> bool:
        values = self.load()
        if name not in values:
            return False
        del values[name]
        self.save(values)
        return True

    def names(self) -> list[str]:
        return sorted(self.load())
