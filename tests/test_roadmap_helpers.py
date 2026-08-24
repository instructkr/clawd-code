from __future__ import annotations

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
NEXT_ID = REPO_ROOT / 'scripts' / 'roadmap-next-id.sh'
DOGFOOD_PROBE = REPO_ROOT / 'scripts' / 'dogfood-probe.py'




def run_next_id(roadmap: Path, script: Path = NEXT_ID) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ['bash', str(script), str(roadmap)],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )


def run_dogfood_probe(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ['python3', str(DOGFOOD_PROBE), *args],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )


class RoadmapHelperTests(unittest.TestCase):
    def test_roadmap_next_id_prints_only_next_id_after_duplicate_check(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            roadmap = Path(temp_dir) / 'ROADMAP.md'
            roadmap.write_text('721. old\n723. helper era\n724. guard\n')

            result = run_next_id(roadmap)

        self.assertEqual(0, result.returncode)
        self.assertEqual('725\n', result.stdout)
        self.assertEqual('', result.stderr)

    def test_roadmap_next_id_fails_fast_on_helper_era_duplicate(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            roadmap = Path(temp_dir) / 'ROADMAP.md'
            roadmap.write_text('722. legacy\n999. first\n999. duplicate\n')

            result = run_next_id(roadmap)

        self.assertNotEqual(0, result.returncode)
        self.assertEqual('', result.stdout)
        self.assertIn('duplicate ROADMAP numeric id(s)', result.stderr)
        self.assertIn('999', result.stderr)
        self.assertNotIn('1000', result.stdout)

    def test_roadmap_next_id_fails_when_explicit_roadmap_path_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            roadmap = Path(temp_dir) / 'missing-ROADMAP.md'

            result = run_next_id(roadmap)

        self.assertNotEqual(0, result.returncode)
        self.assertEqual('', result.stdout)
        self.assertIn('ROADMAP not found', result.stderr)
        self.assertIn(str(roadmap), result.stderr)

    def test_roadmap_next_id_fails_closed_when_checker_is_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            script_dir = Path(temp_dir) / 'scripts'
            script_dir.mkdir()
            copied_next_id = script_dir / 'roadmap-next-id.sh'
            shutil.copy2(NEXT_ID, copied_next_id)
            roadmap = Path(temp_dir) / 'ROADMAP.md'
            roadmap.write_text('724. guard\n')

            result = run_next_id(roadmap, copied_next_id)

        self.assertNotEqual(0, result.returncode)
        self.assertEqual('', result.stdout)
        self.assertIn('required ROADMAP id checker not found or not readable', result.stderr)
        self.assertIn('refusing to print a next id', result.stderr)

    def test_dogfood_probe_runs_explicit_argv_and_separates_channels(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = Path(temp_dir) / 'fixture.py'
            fixture.write_text(
                'from __future__ import annotations\n'
                'import json\n'
                'import sys\n'
                'print(json.dumps({"argv": sys.argv[1:]}))\n'
                'print("diagnostic", file=sys.stderr)\n'
            )

            result = run_dogfood_probe([
                '--stdout-json-byte0',
                '--',
                'python3',
                str(fixture),
                '--output-format',
                'json',
                'doctor',
                '--help',
            ])

        self.assertEqual(0, result.returncode)
        payload = __import__('json').loads(result.stdout)
        self.assertEqual('ok', payload['kind'])
        self.assertEqual([
            'python3',
            str(fixture),
            '--output-format',
            'json',
            'doctor',
            '--help',
        ], payload['argv'])
        self.assertEqual(0, payload['returncode'])
        self.assertEqual('{"argv": ["--output-format", "json", "doctor", "--help"]}\n', payload['stdout'])
        self.assertEqual('diagnostic\n', payload['stderr'])

    def test_dogfood_probe_labels_timeout_separately_from_product_error(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = Path(temp_dir) / 'sleep.py'
            fixture.write_text('import time\ntime.sleep(2)\n')

            result = run_dogfood_probe(['--timeout', '0.1', '--', 'python3', str(fixture)])

        self.assertEqual(1, result.returncode)
        payload = __import__('json').loads(result.stdout)
        self.assertEqual('timeout', payload['kind'])
        self.assertIsNone(payload['returncode'])
        self.assertIn('timed out', payload['message'])

    def test_dogfood_probe_labels_probe_construction_failure(self) -> None:
        result = run_dogfood_probe([])

        self.assertEqual(1, result.returncode)
        payload = __import__('json').loads(result.stdout)
        self.assertEqual('probe_error', payload['kind'])
        self.assertEqual([], payload['argv'])
        self.assertIsNone(payload['returncode'])
        self.assertIn('argv must contain', payload['message'])

    def test_dogfood_probe_labels_stdout_json_prefix_failure_as_product_error(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            fixture = Path(temp_dir) / 'prefixed.py'
            fixture.write_text('print("warning before json")\nprint("{}")\n')

            result = run_dogfood_probe(['--stdout-json-byte0', '--', 'python3', str(fixture)])

        self.assertEqual(1, result.returncode)
        payload = __import__('json').loads(result.stdout)
        self.assertEqual('product_error', payload['kind'])
        self.assertEqual(0, payload['returncode'])
        self.assertIn('byte 0', payload['message'])

if __name__ == '__main__':
    unittest.main()
