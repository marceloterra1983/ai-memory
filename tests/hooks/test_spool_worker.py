#!/usr/bin/env python3
"""Exercise the actual script producers against the unchanged receipt worker."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'local-reliable-mcp'))
from tests import test_hook_delivery as fixtures


class ScriptEnvelopeCompatibility(unittest.TestCase):
    def check_producer(self, engine):
        for token in (None, 'synthetic-token'):
            with self.subTest(engine=engine, static=bool(token)):
                fixture = fixtures.HookDeliveryCase()
                fixture.setUp()
                try:
                    env = dict(os.environ, AI_MEMORY_DATA_DIR=str(fixture.root))
                    env.pop('AI_MEMORY_AUTH_TOKEN', None)
                    if token:
                        env['AI_MEMORY_AUTH_TOKEN'] = token
                    key = 'script-static' if token else 'script-anonymous'
                    url = fixtures.ORIGIN + '/hook?event=stop&agent=codex&workspace=workspace-a&project=demo&ingest_key=' + key
                    body = '{"session_id":"synthetic-script","message":"café <&>"}'
                    if engine == 'sh':
                        command = ['sh', '-c', '. "$1"; ai_memory_spool_event "$2" "$3"', 'probe', str(ROOT / 'hooks/_lib.sh'), url, body]
                    else:
                        env.update(AM_TEST_PS_LIB=str(ROOT / 'hooks/lib/ai-memory-hook.ps1'), AM_TEST_URL=url, AM_TEST_BODY=body)
                        command = ['pwsh', '-NoProfile', '-Command', '. $env:AM_TEST_PS_LIB; Write-AiMemorySpoolEvent -Url $env:AM_TEST_URL -Body $env:AM_TEST_BODY']
                    subprocess.run(command, env=env, check=True, capture_output=True)
                    paths = list(fixture.spool.glob('*.json'))
                    self.assertEqual(len(paths), 1)
                    path = paths[0]
                    raw = path.read_bytes()
                    client = fixtures.CheckpointClient(fixture, complete=False)
                    worker = fixture.drainer(client)
                    result = worker.drain_once()
                    self.assertEqual(result['errors'], 0)
                    self.assertEqual(result['unconfirmed'], 1)
                    self.assertEqual(path.read_bytes(), raw, 'HTTP success without receipt must preserve source')
                    self.assertEqual(client.sent[0][0], key)
                    self.assertEqual(client.sent[0][2], json.loads(body))
                    self.assertEqual(client.sent[0][1].get('Authorization'), 'Bearer ' + token if token else None)
                    fixture.checkpoint(key)
                    self.assertEqual(worker.drain_once()['delivered'], 1)
                    self.assertFalse(path.exists())
                    self.assertEqual((fixture.archive / path.name).read_bytes(), raw)
                    self.assertIsNotNone(json.loads(fixture.receipt_path(path).read_text())['completed_at'])
                finally:
                    fixture.tearDown()

    def test_shell_envelopes_are_accepted(self):
        self.check_producer('sh')

    @unittest.skipUnless(shutil.which('pwsh'), 'pwsh unavailable; static PS checks remain in test_lib.sh')
    def test_powershell_envelopes_are_accepted(self):
        self.check_producer('pwsh')


if __name__ == '__main__':
    unittest.main()
