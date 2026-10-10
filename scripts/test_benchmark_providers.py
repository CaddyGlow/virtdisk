"""Correctness checks for provider evidence handling; no timing assertions."""
import importlib.util
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('providers', Path(__file__).with_name('benchmark-providers.py'))
providers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(providers)


class HarnessTests(unittest.TestCase):
    def test_paired_statistics_and_failed_samples(self):
        rows = [dict(provider=p, sample=i, status='ok', warmup=False,
                     mib_per_second=value, seconds=1/value)
                for i in range(10) for p, value in [('virtdisk', 20), ('qemu', 10)]]
        rows.append(dict(provider='qemu', sample=10, status='failed', warmup=False))
        summary, ratios = providers.summarize(rows, ['virtdisk', 'qemu'], 42)
        self.assertEqual(summary['qemu']['failed_samples'], 1)
        self.assertEqual(ratios['qemu']['bootstrap_95_percent_interval'], [2, 2])
        self.assertEqual(ratios['qemu']['paired_rounds'], 10)

    def test_checkpoint_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            report = root / 'report.json'
            providers.checkpoint(report, {'status': 'running'})
            self.assertIn('running', report.read_text())
            staged, destination = root / 'staged', root / 'destination'
            staged.write_bytes(b'new')
            destination.write_bytes(b'existing')
            with self.assertRaises(FileExistsError):
                providers.publish_no_overwrite(staged, destination)
            self.assertEqual(destination.read_bytes(), b'existing')
            destination.unlink()
            providers.publish_no_overwrite(staged, destination)
            self.assertEqual(destination.read_bytes(), b'new')
            self.assertFalse(staged.exists())

    def test_preexisting_running_report_is_untouched(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / 'report.json'
            original = '{"status": "running", "user": "existing evidence"}'
            report.write_text(original)
            result = subprocess.run([sys.executable, str(Path(__file__).with_name('benchmark-providers.py')),
                                     '--virtdisk', '/does/not/exist', '--output', str(report)],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(report.read_text(), original)

    def test_timeout_and_nonzero_diagnostics(self):
        original = providers.TIMEOUT
        try:
            providers.TIMEOUT = 0.01
            with self.assertRaises(RuntimeError):
                providers.run([sys.executable, '-c', 'import time; time.sleep(2)'])
            self.assertEqual(providers.COMMAND_LOG[-1]['status'], 'timeout')
            providers.TIMEOUT = 10
            with self.assertRaises(RuntimeError):
                providers.run([sys.executable, '-c', 'import sys; print("failure", file=sys.stderr); sys.exit(3)'])
            self.assertEqual(providers.COMMAND_LOG[-1]['returncode'], 3)
            self.assertIn('failure', providers.COMMAND_LOG[-1]['stderr'])
        finally:
            providers.TIMEOUT = original


if __name__ == '__main__':
    unittest.main()
