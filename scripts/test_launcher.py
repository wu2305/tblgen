#!/usr/bin/env python3
"""Launcher updater smoke tests for the packaged tblgen run.py template."""

import hashlib
import importlib.util
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import unittest
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

TOOL = 'tblgen'
ENV = 'TBLGEN'
ROOT = Path(__file__).resolve().parents[1]
RUN_PY = ROOT / 'skill' / TOOL / 'run.py'
def sha256(data):
    return hashlib.sha256(data).hexdigest()


class LauncherTest(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: shutil.rmtree(self.work, ignore_errors=True))
        self.seed = self.work / 'seed' / TOOL
        self.seed.mkdir(parents=True)
        shutil.copy(RUN_PY, self.seed / 'run.py')
        self.cache = self.work / 'cache'
        self.write_release('v0.0.1', '0' * 64)

    def write_release(self, version, linux_digest):
        digests = {
            'linux-amd64': linux_digest,
            'macos-arm64': '0' * 64,
            'windows-amd64.exe': '0' * 64,
        }
        (self.seed / 'release.json').write_text(json.dumps({
            'version': version,
            'sha256': digests,
        }), encoding='utf-8')

    def env(self, mirror):
        env = os.environ.copy()
        env.update({
            f'{ENV}_MIRROR': mirror,
            f'{ENV}_CACHE_DIR': str(self.cache),
            f'{ENV}_UPDATE_TTL_SECS': '0',
        })
        env.pop(f'{ENV}_BIN', None)
        env.pop(f'{ENV}_AUTO_UPDATE', None)
        return env

    def run_launcher(self, env, *args):
        return subprocess.run(
            [sys.executable, str(self.seed / 'run.py'), *args],
            env=env,
            text=True,
            capture_output=True,
            timeout=20,
        )

    def make_zip(self, version, run_body):
        path = self.work / f'{TOOL}-skill-{version}.zip'
        release = json.dumps({
            'version': version,
            'sha256': {
                'linux-amd64': '0' * 64,
                'macos-arm64': '0' * 64,
                'windows-amd64.exe': '0' * 64,
            },
        })
        with zipfile.ZipFile(path, 'w') as z:
            z.writestr(f'{TOOL}/run.py', run_body)
            z.writestr(f'{TOOL}/release.json', release)
        return path.read_bytes()

    def server(self, zipdata, digest=None):
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == '/releases':
                    chosen_digest = digest or sha256(zipdata)
                    data = json.dumps([{
                        'tag_name': 'v9.9.9',
                        'draft': False,
                        'prerelease': False,
                        'assets': [{
                            'name': f'{TOOL}-skill-v9.9.9.zip',
                            'hash_algo': 'sha256',
                            'hash_value': chosen_digest,
                        }],
                    }]).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                elif self.path == f'/releases/download/v9.9.9/{TOOL}-skill-v9.9.9.zip':
                    self.send_response(200)
                    self.send_header('Content-Length', str(len(zipdata)))
                    self.end_headers()
                    self.wfile.write(zipdata)
                else:
                    self.send_error(404)

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join)
        self.addCleanup(server.shutdown)
        return server

    def test_tracks_latest_and_delegates(self):
        zipdata = self.make_zip('v9.9.9', 'import sys\nprint("new-run:"+" ".join(sys.argv[1:]))\n')
        server = self.server(zipdata)
        mirror = f'http://127.0.0.1:{server.server_port}/releases/download'
        result = self.run_launcher(self.env(mirror), 'schema')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), 'new-run:schema')
        self.assertIn(f'downloading {TOOL}-skill-v9.9.9.zip', result.stderr)
        self.assertTrue((self.cache / 'skill' / 'v9.9.9' / TOOL / 'run.py').is_file())

    def test_hash_mismatch_refuses_install(self):
        zipdata = self.make_zip('v9.9.9', 'print("unreachable")\n')
        server = self.server(zipdata + b'corrupt', digest=sha256(zipdata))
        mirror = f'http://127.0.0.1:{server.server_port}/releases/download'
        result = self.run_launcher(self.env(mirror), 'schema')
        self.assertEqual(result.returncode, 2)
        self.assertIn('SHA-256 mismatch', result.stderr)
        self.assertFalse((self.cache / 'skill' / 'v9.9.9').exists())

    def test_incomplete_newest_release_is_skipped(self):
        spec = importlib.util.spec_from_file_location('launcher_under_test', RUN_PY)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        releases = [
            {'tag_name': 'v9.9.9', 'draft': False, 'prerelease': False, 'assets': []},
            {
                'tag_name': 'v9.9.8',
                'draft': False,
                'prerelease': False,
                'assets': [{'name': f'{TOOL}-skill-v9.9.8.zip'}],
            },
        ]
        self.assertEqual(module.latest_release(releases)['tag_name'], 'v9.9.8')

    @unittest.skipUnless(sys.platform.startswith('linux'), 'uses a shell-script cached binary')
    def test_release_index_failure_falls_back_to_cached_current_binary(self):
        binary = b'#!/bin/sh\necho current-binary\n'
        digest = sha256(binary)
        self.write_release('v0.0.1', digest)
        folder = self.cache / 'v0.0.1'
        folder.mkdir(parents=True)
        cached = folder / f'{TOOL}-v0.0.1-linux-amd64'
        cached.write_bytes(binary)
        cached.chmod(cached.stat().st_mode | stat.S_IXUSR)
        env = self.env('http://127.0.0.1:9/releases/download')
        result = self.run_launcher(env, 'schema')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), 'current-binary')
        self.assertIn('latest-release check failed', result.stderr)


if __name__ == '__main__':
    unittest.main()
