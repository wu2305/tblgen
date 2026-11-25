#!/usr/bin/env python3
"""Package the tblgen agent skill into a release bundle zip.

Usage (any working directory):
    python3 scripts/package_skill.py TAG DIST

TAG  release tag, e.g. v0.1.1
DIST directory containing the three platform binaries built by CI:
     tblgen-<TAG>-linux-amd64
     tblgen-<TAG>-macos-arm64
     tblgen-<TAG>-windows-amd64.exe

Produces DIST/tblgen-skill-<TAG>.zip containing exactly:
    tblgen/SKILL.md, tblgen/run.py, tblgen/release.json,
    tblgen/examples/<name>.spec.json for each name in EXAMPLES
in that fixed order, stored uncompressed with stable 1980-01-01 timestamps.
The manifest release.json pins the tag and the SHA-256 of each binary.
Existing assets are never modified; the zip is created exclusively.
"""

import hashlib
import json
import re
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SKILL_DIR = ROOT / 'skill' / 'tblgen'

# binary filename suffix == manifest sha256 key (matches run.py PLATFORMS)
ASSETS = ['linux-amd64', 'macos-arm64', 'windows-amd64.exe']
# business examples referenced by SKILL.md, shipped under tblgen/examples/
EXAMPLES = ['budget', 'property-model-long']
TAG_RE = re.compile(r'v\d+\.\d+\.\d+')


def sha256_file(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(65536), b''):
            h.update(block)
    return h.hexdigest()


def main(argv):
    if len(argv) != 3:
        print('usage: package_skill.py TAG DIST', file=sys.stderr)
        return 2
    tag, dist = argv[1], Path(argv[2])
    if not TAG_RE.fullmatch(tag):
        print(f'invalid tag {tag!r}; expected vX.Y.Z', file=sys.stderr)
        return 2
    if not dist.is_dir():
        print(f'DIST is not a directory: {dist}', file=sys.stderr)
        return 2

    digests = {}
    for suffix in ASSETS:
        binary = dist / f'tblgen-{tag}-{suffix}'
        if not binary.is_file():
            print(f'missing binary: {binary}', file=sys.stderr)
            return 2
        if binary.stat().st_size == 0:
            print(f'empty binary: {binary}', file=sys.stderr)
            return 2
        digests[suffix] = sha256_file(binary)

    manifest = json.dumps(
        {'version': tag, 'sha256': digests},
        ensure_ascii=False,
        indent=2,
        sort_keys=True,
    ).encode('utf-8') + b'\n'

    skill_md = SKILL_DIR / 'SKILL.md'
    run_py = SKILL_DIR / 'run.py'
    examples = [ROOT / 'examples' / f'{n}.spec.json' for n in EXAMPLES]
    for p in (skill_md, run_py, *examples):
        if not p.is_file():
            print(f'missing skill file: {p}', file=sys.stderr)
            return 2

    out = dist / f'tblgen-skill-{tag}.zip'
    entries = [
        ('tblgen/SKILL.md', skill_md.read_bytes(), 0o644),
        ('tblgen/run.py', run_py.read_bytes(), 0o755),
        ('tblgen/release.json', manifest, 0o644),
        *((f'tblgen/examples/{p.name}', p.read_bytes(), 0o644) for p in examples),
    ]
    try:
        with zipfile.ZipFile(out, 'x', compression=zipfile.ZIP_STORED) as z:
            for name, data, mode in entries:
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_STORED
                info.create_system = 3
                info.external_attr = (0o100000 | mode) << 16
                z.writestr(info, data)
    except FileExistsError:
        print(f'refusing to overwrite existing zip: {out}', file=sys.stderr)
        return 2
    print(f'wrote {out}')
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
