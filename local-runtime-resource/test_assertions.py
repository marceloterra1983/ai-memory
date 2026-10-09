"""Exercise the rebuild's assertions with small cargo output fixtures."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
CASE = 'integration::retrieval_entity_project_name::entity_stream_does_not_vote_for_the_scoped_project_name'


def output(count=1, name=CASE, failed=False):
    return (f'test {name} ... {"FAILED" if failed else "ok"}\n'
            f'test result: {"FAILED" if failed else "ok"}. '
            f'{0 if failed else count} passed; {count if failed else 0} failed; '
            '0 ignored; 0 measured; 100 filtered out; finished in 0.01s\n')


class Assertions(unittest.TestCase):
    def run_fixture(self, logs, exits, rebuild=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / 'tools'
            tools.mkdir()
            for i, text in enumerate(logs):
                (root / f'log{i}').write_text(text)
                (root / f'exit{i}').write_text(str(exits[i]))
            cargo = tools / 'cargo'
            cargo.write_text('''#!/usr/bin/env python3
import os, pathlib, sys
p = pathlib.Path(os.environ['FIXTURE'])
counter = p / 'counter'
i = int(counter.read_text()) if counter.exists() else 0
counter.write_text(str(i + 1))
print((p / f'log{i}').read_text(), end='')
sys.exit(int((p / f'exit{i}').read_text()))
''')
            cargo.chmod(0o755)
            env = {**os.environ, 'PATH': str(tools) + ':' + os.environ['PATH'], 'FIXTURE': str(root)}
            if rebuild:
                # Run the actual in-container test/mutation/restore section,
                # without package installation or an expensive image build.
                source = (HERE / 'build-reproduce.sh').read_text()
                section = source[source.index('  FILTER='):source.rindex("\n'\n")]
                target = root / 'crates/ai-memory-store/src/reader.rs'
                target.parent.mkdir(parents=True)
                target.write_text('e.name <> ?\n')
                subprocess.run(['git', 'init', '-q', str(root)], check=True)
                subprocess.run(['git', '-C', str(root), 'add', '.'], check=True)
                subprocess.run(['git', '-C', str(root), '-c', 'user.name=Fixture',
                                '-c', 'user.email=fixture@example.invalid', 'commit',
                                '--no-gpg-sign', '-qm', 'fixture'], check=True)
                result = subprocess.run(['bash', '-euo', 'pipefail', '-c', section],
                                        cwd=root, env=env, capture_output=True, text=True)
                self.assertEqual(target.read_text(), 'e.name <> ?\n')
                return result
            return subprocess.run(['bash', str(HERE / 'assert_fixbrainrank.sh'), str(root)],
                                  env=env, capture_output=True, text=True)

    def test_assert_requires_exact_case_and_one_pass(self):
        self.assertEqual(self.run_fixture([output()], [0]).returncode, 0)
        for log, code in [(output(0), 0), (output(11), 0), (output(name='wrong'), 0),
                          ('error: could not compile\n', 101)]:
            with self.subTest(log=log):
                self.assertNotEqual(self.run_fixture([log], [code]).returncode, 0)

    def test_rebuild_requires_exact_case_count_and_real_failure(self):
        self.assertEqual(self.run_fixture([output(), output(failed=True), output()],
                                         [0, 101, 0], True).returncode, 0)
        bad = [(output(0), 0), (output(11), 0), (output(name='wrong'), 0),
               ('error: could not compile\n', 101)]
        for log, code in bad:
            for position in (0, 2):
                with self.subTest(position=position, log=log):
                    logs = [output(), output(failed=True), output()]
                    exits = [0, 101, 0]
                    logs[position], exits[position] = log, code
                    self.assertNotEqual(self.run_fixture(logs, exits, True).returncode, 0)
        for log, code in [(output(), 0), (output(failed=True), 1),
                          (output(name='wrong', failed=True), 101),
                          (output(11, failed=True), 101),
                          ('error: could not compile\n', 101)]:
            with self.subTest(negative=log, code=code):
                self.assertNotEqual(self.run_fixture([output(), log, output()],
                                                    [0, code, 0], True).returncode, 0)


if __name__ == '__main__':
    unittest.main()
