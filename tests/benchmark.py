#!/usr/bin/env python3
"""Exercise benchmark startup failures with real child processes, without LAN access."""
import importlib.util
from pathlib import Path
import queue
import subprocess
import sys
import threading
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts' / 'benchmark.py'
spec = importlib.util.spec_from_file_location('lan_benchmark', SCRIPT)
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class ShortWaitQueue(queue.Queue):
    def get(self, block=True, timeout=None):
        return super().get(block=block, timeout=2)


class BenchmarkTests(unittest.TestCase):
    def start_receiver(self, source):
        processes, readers = [], []
        popen, thread = subprocess.Popen, threading.Thread

        def spawn(*args, **kwargs):
            process = popen([sys.executable, '-u', '-c', source], **kwargs)
            processes.append(process)
            return process

        def start_thread(*args, **kwargs):
            reader = thread(*args, **kwargs)
            readers.append(reader)
            return reader

        try:
            with patch.object(benchmark.subprocess, 'Popen', side_effect=spawn), \
                    patch.object(benchmark.threading, 'Thread', side_effect=start_thread), \
                    patch.object(benchmark.queue, 'Queue', ShortWaitQueue):
                result = benchmark.receiver('unused', 'zig', '/tmp', 'token', '/tmp', 9000)
        except RuntimeError:
            self.assertIsNotNone(processes[0].poll(), 'Failed receiver must be reaped')
            self.assertFalse(readers[0].is_alive(), 'Reader must stop after failure')
            self.assertTrue(processes[0].stdout.closed)
            self.assertTrue(processes[0].stderr.closed)
            raise
        return result

    def test_hung_receiver(self):
        with self.assertRaisesRegex(RuntimeError, 'within 20 seconds'):
            self.start_receiver('import time; time.sleep(60)')

    def test_receiver_exits_without_listening(self):
        with self.assertRaisesRegex(RuntimeError, 'before becoming ready'):
            self.start_receiver("print('startup failed')")

    def test_receiver_closes_stdout_but_keeps_running(self):
        with self.assertRaisesRegex(RuntimeError, 'before becoming ready'):
            self.start_receiver('import os, time; os.close(1); time.sleep(60)')

    def test_ready_receiver_remains_available(self):
        process, reader, lines = self.start_receiver("import time; print('listening on port 9000'); time.sleep(60)")
        try:
            self.assertIsNone(process.poll())
            self.assertIn('listening on port 9000\n', lines)
        finally:
            process.kill()
            process.wait(timeout=10)
            reader.join(timeout=5)
            process.stdout.close()
            process.stderr.close()

    def test_nonpositive_trials_fail_before_startup(self):
        for trials in ('0', '-1'):
            with self.subTest(trials=trials):
                result = subprocess.run([sys.executable, str(SCRIPT), '--root', '/tmp/xfer-benchmark-test', '--trials', trials], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 2)
                self.assertIn('--trials must be positive', result.stderr)


if __name__ == '__main__':
    unittest.main()
