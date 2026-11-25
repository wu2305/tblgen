"""tblgen launcher: fetch the pinned release binary, verify SHA-256, exec it.

This file is a sibling of SKILL.md inside the skill bundle. Invoke it by its
absolute path (the directory may contain spaces), from any working directory:

    python3 "/path/to/tblgen/run.py" schema
    py -3 "C:\\path\\to\\tblgen\\run.py" schema        # Windows

Environment:
    TBLGEN_BIN       use this binary instead of downloading (must match the
                     pinned SHA-256 of the released artifact for this platform)
    TBLGEN_CACHE_DIR override the download cache directory

The version tag and per-platform SHA-256 digests come from release.json,
which sits next to this script inside the bundle. There is no runtime
hash download: upgrade by installing a newer bundle.
"""

import hashlib
import http.client
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

# platform (system, machine) -> release asset suffix; fixed mapping
PLATFORMS = {
    ('Darwin', 'arm64'): 'macos-arm64',
    ('Linux', 'x86_64'): 'linux-amd64',
    ('Windows', 'amd64'): 'windows-amd64.exe',
}
REPO = 'wu2305/tblgen'
VERSION_RE = re.compile(r'v\d+\.\d+\.\d+')
DIGEST_RE = re.compile(r'[0-9a-f]{64}')


def verify(path, expected):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(65536), b''):
            h.update(block)
    if h.hexdigest() != expected:
        raise ValueError(f'SHA-256 mismatch: {path}; refusing to execute')
    return path


def load_release():
    """Read the sibling release.json manifest; returns (version, sha256 map)."""
    path = Path(__file__).with_name('release.json')
    release = json.loads(path.read_text(encoding='utf-8'))
    version = release.get('version')
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise ValueError(f'invalid release version in {path}: {version!r}')
    sha256 = release.get('sha256')
    if not isinstance(sha256, dict):
        raise ValueError(f'missing sha256 map in {path}')
    for target, digest in sha256.items():
        if not isinstance(digest, str) or not DIGEST_RE.fullmatch(digest):
            raise ValueError(f'invalid sha256 for {target} in {path}: {digest!r}')
    return version, sha256


def resolve():
    system = platform.system()
    arch = platform.machine().lower()
    if arch == 'x86_64' and system == 'Windows':
        arch = 'amd64'
    if arch == 'amd64' and system == 'Linux':
        arch = 'x86_64'
    if (system, arch) not in PLATFORMS:
        raise ValueError(
            f'Unsupported platform: {system}/{arch}; '
            'supported: macOS-arm64, Linux-amd64, Windows-amd64'
        )
    target = PLATFORMS[system, arch]
    version, sha256 = load_release()
    digest = sha256.get(target)
    if digest is None:
        raise ValueError(f'release manifest has no digest for {target}')
    if os.environ.get('TBLGEN_BIN'):
        return verify(Path(os.environ['TBLGEN_BIN']).expanduser().resolve(), digest)
    base = os.environ.get('TBLGEN_CACHE_DIR')
    if not base:
        if system == 'Windows':
            parent = (
                os.environ.get('LOCALAPPDATA')
                or Path.home() / 'AppData/Local'
            )
        elif system == 'Darwin':
            parent = Path.home() / 'Library/Caches'
        else:
            parent = os.environ.get('XDG_CACHE_HOME') or Path.home() / '.cache'
        base = Path(parent) / 'tblgen'
    folder = Path(base).expanduser().resolve() / version
    folder.mkdir(parents=True, exist_ok=True)
    name = f'tblgen-{version}-{target}'
    dest = folder / name
    if dest.exists():
        return verify(dest, digest)
    tmp = None
    try:
        url = (
            f'https://cnb.cool/{REPO}/-/releases/download/{version}/{name}'
        )
        with tempfile.NamedTemporaryFile(dir=folder, delete=False) as out:
            tmp = Path(out.name)
            print(f'tblgen: downloading {name}', file=sys.stderr)
            with urllib.request.urlopen(url, timeout=40) as response:
                shutil.copyfileobj(response, out)
        verify(tmp, digest)
        tmp.chmod(0o755)
        os.replace(tmp, dest)
        return dest
    finally:
        if tmp is not None:
            tmp.unlink(missing_ok=True)


def main():
    try:
        return subprocess.run([str(resolve()), *sys.argv[1:]]).returncode
    except (OSError, ValueError, EOFError, http.client.HTTPException) as e:
        print(f'tblgen launcher: {e}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
