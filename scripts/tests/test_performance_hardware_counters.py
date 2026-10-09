#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import io
from pathlib import Path
import signal
import subprocess
import sys
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import performance_hardware_counters as counters
from performance_qualification import QualificationError


def interval(elapsed=1, coverage=100):
    return (f'{elapsed},1000000,,cycles,900000000,{coverage},,\n'
            f'{elapsed},2000000,,instructions,900000000,{coverage},2.0,insn per cycle\n')


class HardwareCounterTest(unittest.TestCase):
    def process(self, data=None):
        process = Mock()
        process.stderr = io.StringIO(interval() if data is None else data)
        process.poll.return_value = None
        process.wait.return_value = 0
        return process

    def test_grouped_counts_retain_coverage_without_normalizing_into_query_work(self):
        observer = counters.HardwareCounters('8')
        for line in ('# header\n' + interval(1, 75) + interval(2)).splitlines():
            observer.consume(line)
        result = observer.finish()
        self.assertIsNone(result['sampling_error'])
        self.assertEqual(len(result['intervals']), 2)
        events = result['intervals'][0]['events']
        self.assertEqual(events['cycles'], {'count': 1000000, 'running_nanoseconds': 900000000,
                                          'running_percent': 75})
        self.assertEqual(events['instructions']['count'], 2000000)

    def test_missing_duplicate_unordered_and_unavailable_events_are_not_valid_evidence(self):
        cases = [
            ('1,1,,cycles,1,100\n1,2,,cycles,1,100', 'duplicate'),
            ('1,1,,cycles,1,100\n2,2,,instructions,1,100', 'incomplete'),
            (interval(2) + interval(1), 'unordered'),
            ('1,<not supported>,,cycles,1,100', 'unavailable'),
            ('1,1,,cycles,1,0', 'invalid'),
            ('nan,1,,cycles,1,100', 'invalid'),
            ('1,-1,,cycles,1,100', 'invalid'),
            ('1,1,,cycles,1,101', 'invalid'),
            ('1,1,,cycles,0,100', 'invalid'),
            ('1,1,,cycles,1,nan', 'invalid'),
            ('1,1,,branches,1,100', 'unexpected'),
        ]
        for raw, message in cases:
            with self.subTest(raw=raw):
                observer = counters.HardwareCounters('8')
                with self.assertRaisesRegex(QualificationError, message):
                    for line in raw.splitlines():
                        observer.consume(line)
        observer = counters.HardwareCounters('8')
        observer.consume('1,1,,cycles,1,100')
        self.assertIn('incomplete', observer.finish()['sampling_error'])

    def test_interval_and_stream_bounds_reject_excess_output(self):
        with patch.object(counters, 'MAX_INTERVALS', 2):
            observer = counters.HardwareCounters('8')
            for elapsed in (0.25, 0.5, 0.75):
                for line in interval(elapsed).splitlines():
                    observer.consume(line)
            with self.assertRaisesRegex(QualificationError, 'inventory exceeded'):
                observer.consume('1,1,,cycles,1,100')
            for raw in ['x' * (counters.MAX_LINE + 1), '# header\n' * 23]:
                observer = counters.HardwareCounters('8')
                observer.process = self.process(raw)
                observer.observe()
                self.assertIn('exceeded its bound', observer.error)

    def test_observer_counts_only_reserved_cpus_and_stops_without_starting_a_workload(self):
        process = self.process()
        with patch.object(counters.subprocess, 'Popen', return_value=process) as launch:
            observer = counters.HardwareCounters('8-15')
            observer.start()
            result = observer.finish()
        self.assertIsNone(result['sampling_error'])
        command = launch.call_args.args[0]
        self.assertEqual(command[command.index('-C') + 1], '8-15')
        self.assertEqual(command[command.index('-e') + 1], '{cycles,instructions}')
        self.assertIn('--no-scale', command)
        self.assertIn('--interval-count', command)
        self.assertNotIn('--', command)
        self.assertEqual(launch.call_args.kwargs['env']['LC_ALL'], 'C')
        process.send_signal.assert_called_once_with(signal.SIGINT)
        self.assertTrue(process.stderr.closed)

    def test_early_exit_and_observer_timeout_cannot_claim_complete_evidence(self):
        for outcome in ('early', 'timeout', 'error'):
            process = self.process()
            if outcome == 'early':
                process.poll.return_value = 0
            elif outcome == 'timeout':
                process.wait.side_effect = [subprocess.TimeoutExpired('perf', 5), -9]
            else:
                process.wait.return_value = 1
            with self.subTest(outcome=outcome), patch.object(counters.subprocess, 'Popen', return_value=process):
                observer = counters.HardwareCounters('8')
                observer.start()
                self.assertIsNotNone(observer.finish()['sampling_error'])
                if outcome == 'timeout':
                    process.kill.assert_called_once()


if __name__ == '__main__':
    unittest.main()
