"""tblgen launcher: track the latest release bundle, fetch its pinned binary, verify SHA-256, exec it.

This file is a sibling of SKILL.md inside the skill bundle. Invoke it by its
absolute path (the directory may contain spaces), from any working directory:

    python3 "/path/to/tblgen/run.py" schema
    py -3 "C:\\path\\to\\tblgen\\run.py" schema        # Windows

The launcher tracks the remote latest release by default. Before running a
command it checks the release index at most once per hour, downloads a different
latest skill bundle into the local cache, and delegates to
that bundle's run.py. Use `python3 run.py update` to force the check.

Environment:
    TBLGEN_BIN       use this binary instead of downloading (must match the
                     pinned SHA-256 of the released artifact for this platform);
                     also disables update tracking because it is a local override
    TBLGEN_CACHE_DIR override the download cache directory
    TBLGEN_MIRROR    download base URL replacing DEFAULT_MIRROR; the mirror
                     must preserve the CNB release layout: the JSON release index is
                     the mirror base without the trailing /download, and assets are
                     served as <mirror>/<version>/<asset>
    TBLGEN_AUTO_UPDATE
                     set to 0/false/no/off to disable automatic latest tracking
    TBLGEN_UPDATE_TTL_SECS
                     seconds between release-index checks (default 3600; 0 checks
                     every invocation)

The version tag and per-platform SHA-256 digests for the binary come from
release.json, which sits next to this script inside the bundle. Skill zip
downloads are verified against the SHA-256 published in the release metadata.
There is no hard-coded consumer-side checksum.
"""

import hashlib
import http.client
import io
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
import zipfile
from pathlib import Path, PurePosixPath

TOOL = 'tblgen'
ENV = 'TBLGEN'
# platform (system, machine) -> release asset suffix; fixed mapping
PLATFORMS = {
    ('Darwin', 'arm64'): 'macos-arm64',
    ('Linux', 'x86_64'): 'linux-amd64',
    ('Windows', 'amd64'): 'windows-amd64.exe',
}
DEFAULT_MIRROR = 'https://cnb.cool/wu2305/tblgen/-/releases/download'
VERSION_RE = re.compile(r'v\d+\.\d+\.\d+')
DIGEST_RE = re.compile(r'[0-9a-f]{64}')
DEFAULT_UPDATE_TTL_SECS = 3600
DOWNLOAD_ATTEMPTS = 3


class UpdateIntegrityError(ValueError):
    pass


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


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


def cache_root(system):
    base = os.environ.get(f'{ENV}_CACHE_DIR')
    if base:
        return Path(base).expanduser().resolve()
    if system == 'Windows':
        parent = (
            os.environ.get('LOCALAPPDATA')
            or Path.home() / 'AppData/Local'
        )
    elif system == 'Darwin':
        parent = Path.home() / 'Library/Caches'
    else:
        parent = os.environ.get('XDG_CACHE_HOME') or Path.home() / '.cache'
    return Path(parent) / TOOL


def mirror_base():
    return (os.environ.get(f'{ENV}_MIRROR') or DEFAULT_MIRROR).rstrip('/')


def release_index_url(mirror):
    # DEFAULT_MIRROR is <releases>/download. The JSON index is <releases>.
    if mirror.endswith('/download'):
        return mirror[:-len('/download')]
    return f'{mirror}/-/releases'


def auto_update_enabled():
    value = os.environ.get(f'{ENV}_AUTO_UPDATE', '1').strip().lower()
    return value not in {'0', 'false', 'no', 'off'}


def update_ttl():
    raw = os.environ.get(f'{ENV}_UPDATE_TTL_SECS')
    if raw is None or raw == '':
        return DEFAULT_UPDATE_TTL_SECS
    try:
        ttl = int(raw)
    except ValueError:
        raise ValueError(f'{ENV}_UPDATE_TTL_SECS must be a non-negative integer')
    if ttl < 0:
        raise ValueError(f'{ENV}_UPDATE_TTL_SECS must be a non-negative integer')
    return ttl


def state_path(base):
    return base / 'skill' / 'latest-release-state.json'


def read_state(path, mirror):
    try:
        data = json.loads(path.read_text(encoding='utf-8'))
    except (OSError, ValueError):
        return None
    if data.get('mirror') != mirror or not isinstance(data.get('release'), dict):
        return None
    return data


def write_state(path, mirror, release):
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix('.tmp')
        tmp.write_text(json.dumps({
            'checked_at': int(time.time()),
            'mirror': mirror,
            'release': release,
        }, ensure_ascii=False), encoding='utf-8')
        os.replace(tmp, path)
    except OSError:
        pass


def fetch_release_index(mirror):
    url = release_index_url(mirror)
    req = urllib.request.Request(url, headers={'Accept': 'application/json'})
    with urllib.request.urlopen(req, timeout=20) as response:
        releases = json.loads(response.read().decode('utf-8'))
    if not isinstance(releases, list):
        raise ValueError(f'release index is not a list: {url}')
    return releases


def semver_key(tag):
    return tuple(int(part) for part in tag[1:].split('.'))


def has_skill_asset(release):
    tag = release.get('tag_name')
    if not isinstance(tag, str) or not VERSION_RE.fullmatch(tag):
        return False
    expected = f'{TOOL}-skill-{tag}.zip'
    assets = release.get('assets')
    return isinstance(assets, list) and any(
        isinstance(asset, dict) and asset.get('name') == expected
        for asset in assets
    )


def latest_release(releases):
    candidates = []
    for release in releases:
        if not isinstance(release, dict):
            continue
        tag = release.get('tag_name')
        if (
            isinstance(tag, str)
            and VERSION_RE.fullmatch(tag)
            and not release.get('draft')
            and not release.get('prerelease')
            and has_skill_asset(release)
        ):
            candidates.append(release)
    if not candidates:
        raise ValueError('release index has no stable vX.Y.Z release with a skill zip')
    return max(candidates, key=lambda item: semver_key(item['tag_name']))


def find_skill_asset(release):
    tag = release['tag_name']
    expected = f'{TOOL}-skill-{tag}.zip'
    assets = release.get('assets')
    if not isinstance(assets, list):
        raise ValueError(f'release {tag} has no asset list')
    for asset in assets:
        if not isinstance(asset, dict) or asset.get('name') != expected:
            continue
        digest = asset.get('hash_value')
        algo = asset.get('hash_algo')
        if algo != 'sha256' or not isinstance(digest, str) or not DIGEST_RE.fullmatch(digest):
            raise ValueError(f'release {tag} has no SHA-256 metadata for {expected}')
        return expected, digest
    raise ValueError(f'release {tag} has no asset {expected}')


def safe_extract_skill_zip(data, target_parent):
    try:
        archive = zipfile.ZipFile(io.BytesIO(data))
    except zipfile.BadZipFile as e:
        raise UpdateIntegrityError(f'invalid skill zip: {e}') from e
    for info in archive.infolist():
        name = info.filename
        parts = PurePosixPath(name).parts
        if (
            not parts
            or parts[0] != TOOL
            or name.startswith('/')
            or '\\' in name
            or '..' in parts
        ):
            raise UpdateIntegrityError(f'unsafe path in skill zip: {name!r}')
    archive.extractall(target_parent)
    if not (target_parent / TOOL / 'run.py').is_file():
        raise UpdateIntegrityError(f'skill zip lacks {TOOL}/run.py')
    if not (target_parent / TOOL / 'release.json').is_file():
        raise UpdateIntegrityError(f'skill zip lacks {TOOL}/release.json')


def install_skill_zip(base, tag, name, digest, mirror):
    skill_root = base / 'skill'
    dest = skill_root / tag
    run_py = dest / TOOL / 'run.py'
    release_json = dest / TOOL / 'release.json'
    if run_py.is_file() and release_json.is_file():
        return run_py
    skill_root.mkdir(parents=True, exist_ok=True)
    url = f'{mirror}/{tag}/{name}'
    tmp = None
    stage = None
    try:
        with tempfile.NamedTemporaryFile(dir=skill_root, delete=False) as out:
            tmp = Path(out.name)
            print(f'{TOOL}: downloading {name}', file=sys.stderr)
            with urllib.request.urlopen(url, timeout=40) as response:
                shutil.copyfileobj(response, out)
        data = tmp.read_bytes()
        if sha256_bytes(data) != digest:
            raise UpdateIntegrityError(f'{TOOL} updater: {name} SHA-256 mismatch, refusing to install')
        stage = Path(tempfile.mkdtemp(dir=skill_root))
        safe_extract_skill_zip(data, stage)
        shutil.rmtree(dest, ignore_errors=True)
        os.replace(stage, dest)
        stage = None
        return run_py
    finally:
        if tmp is not None:
            tmp.unlink(missing_ok=True)
        if stage is not None:
            shutil.rmtree(stage, ignore_errors=True)


def cached_or_remote_latest(base, mirror, force):
    path = state_path(base)
    state = read_state(path, mirror)
    now = time.time()
    try:
        checked_at = int(state.get('checked_at') or 0) if state is not None else 0
    except (TypeError, ValueError):
        checked_at = 0
    if not force and state is not None and now - checked_at < update_ttl():
        return state['release'], False
    try:
        release = latest_release(fetch_release_index(mirror))
    except (OSError, ValueError, json.JSONDecodeError, http.client.HTTPException):
        if force:
            raise
        if state is not None:
            return state['release'], False
        raise
    write_state(path, mirror, release)
    return release, True


def check_latest(force=False):
    """Return (run.py path, tag) when the remote latest bundle differs."""
    if os.environ.get(f'{ENV}_BIN'):
        return None
    if not force and not auto_update_enabled():
        return None
    system = platform.system()
    base = cache_root(system)
    mirror = mirror_base()
    current, _ = load_release()
    try:
        release, _fresh = cached_or_remote_latest(base, mirror, force)
    except (OSError, ValueError, json.JSONDecodeError, http.client.HTTPException) as e:
        if force:
            raise
        print(f'{TOOL} launcher: latest-release check failed ({e}); using {current}', file=sys.stderr)
        return None
    tag = release['tag_name']
    if tag == current:
        return None
    try:
        name, digest = find_skill_asset(release)
        run_py = install_skill_zip(base, tag, name, digest, mirror)
    except UpdateIntegrityError:
        raise
    except (OSError, ValueError, http.client.HTTPException) as e:
        if force:
            raise
        print(f'{TOOL} launcher: latest release is not installable ({e}); using {current}', file=sys.stderr)
        return None
    return run_py, tag


def resolve_binary():
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
    if os.environ.get(f'{ENV}_BIN'):
        return verify(Path(os.environ[f'{ENV}_BIN']).expanduser().resolve(), digest)
    base = cache_root(system)
    folder = base / version
    folder.mkdir(parents=True, exist_ok=True)
    name = f'{TOOL}-{version}-{target}'
    dest = folder / name
    if dest.exists():
        return verify(dest, digest)
    mirror = mirror_base()
    url = f'{mirror}/{version}/{name}'
    for attempt in range(1, DOWNLOAD_ATTEMPTS + 1):
        try:
            return download_binary(url, folder, name, dest, digest)
        except (OSError, ValueError, http.client.HTTPException) as e:
            # a dropped connection can end the body early without an error;
            # the length/hash checks catch it, so retry a few times
            if attempt == DOWNLOAD_ATTEMPTS:
                raise
            print(f'{TOOL}: download failed ({e}); retrying', file=sys.stderr)


def download_binary(url, folder, name, dest, digest):
    tmp = None
    try:
        with tempfile.NamedTemporaryFile(dir=folder, delete=False) as out:
            tmp = Path(out.name)
            print(f'{TOOL}: downloading {name}', file=sys.stderr)
            with urllib.request.urlopen(url, timeout=40) as response:
                shutil.copyfileobj(response, out)
                expected = response.headers.get('Content-Length')
            size = out.tell()
        if expected is not None and expected.isdigit() and int(expected) != size:
            raise ValueError(f'incomplete download of {name}: {size} of {expected} bytes')
        verify(tmp, digest)
        tmp.chmod(0o755)
        os.replace(tmp, dest)
        return dest
    finally:
        if tmp is not None:
            tmp.unlink(missing_ok=True)


def main():
    args = sys.argv[1:]
    try:
        if args and args[0] == 'update':
            current, _ = load_release()
            result = check_latest(force=True)
            if result is None:
                print(f'{TOOL} launcher: already tracking latest {current}')
            else:
                run_py, tag = result
                print(f'{TOOL} launcher: installed latest {tag} at {run_py.parent.parent}; future commands use it')
            return 0
        result = check_latest()
        if result is not None:
            run_py, _tag = result
            return subprocess.run([sys.executable, str(run_py), *args]).returncode
        return subprocess.run([str(resolve_binary()), *args]).returncode
    except (OSError, ValueError, EOFError, http.client.HTTPException, json.JSONDecodeError) as e:
        print(f'{TOOL} launcher: {e}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
