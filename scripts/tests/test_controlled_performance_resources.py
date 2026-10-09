#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import json
from pathlib import Path
import sys
import tempfile
import unittest
from contextlib import nullcontext
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import controlled_performance_resources as resources
import controlled_runner_host as runner
from performance_qualification import QualificationError


class ResourceEvidenceTest(unittest.TestCase):
    def setUp(self):
        self.hardware = SimpleNamespace(start=lambda: None, finish=lambda: {'sampling_error': None})
        patcher = patch.object(resources, 'HardwareCounters', return_value=self.hardware)
        patcher.start()
        self.addCleanup(patcher.stop)

    def layout_fixture(self, root):
        executable = (root / 'benchmark').resolve()
        executable.touch()
        process = root / 'proc' / '100'
        process.mkdir(parents=True)
        (process / 'exe').symlink_to(executable)
        fields = ['0'] * 50
        fields[19] = '123'
        (process / 'stat').write_text('100 (benchmark) ' + ' '.join(fields))
        (process / 'personality').write_text('00040000\n')
        (process / 'maps').write_text(f'1000-2000 r-xp 0000 00:00 1 {executable}\n'
                                      '3000-4000 rw-p 0000 00:00 0 [heap]\n')
        return executable, process

    def test_fixed_layout_is_observed_and_later_randomization_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            executable, process = self.layout_fixture(root)
            with patch.object(resources, 'PROC', process.parent):
                monitor = resources.MeasurementResources(root, root / 'result.json', '8', executable=executable)
                monitor.sample_layout('100')
                row = monitor.layouts['100/123']
                self.assertEqual(row['personality'], '00040000')
                self.assertEqual(len(row['executable_mappings']), 1)
                self.assertEqual(len(row['heap_and_stack']), 1)
                (process / 'personality').write_text('00000000\n')
                with self.assertRaisesRegex(QualificationError, 'randomization is enabled'):
                    monitor.sample_layout('100')

    def test_missing_or_oversized_executable_mappings_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            executable, process = self.layout_fixture(root)
            with patch.object(resources, 'PROC', process.parent):
                for mappings, message in [('1000-2000 rw-p 0000 00:00 0 [heap]\n', 'mapping evidence is missing'),
                                          ('x' * (256 * 1024 + 1), 'exceeded its bound')]:
                    (process / 'maps').write_text(mappings)
                    monitor = resources.MeasurementResources(root, root / 'result.json', '8', executable=executable)
                    with self.subTest(message=message), self.assertRaisesRegex(QualificationError, message):
                        monitor.sample_layout('100')

    def test_task_counters_cannot_replace_missing_layout_evidence(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            monitor = resources.MeasurementResources(root, root / 'result.json', '8', executable=root / 'benchmark')
            monitor.tasks['100/100/123'] = {}
            with patch.object(monitor, 'sample'), self.assertRaisesRegex(QualificationError, 'no fixed workload layout'):
                with monitor:
                    pass
            self.assertIn('no fixed workload layout', json.loads((root / 'result.json').read_text())['sampling_error'])

    def test_only_measurement_processes_disable_address_randomization(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            host = runner.ControlledHost.__new__(runner.ControlledHost)
            host.output, host.run_id, host.counter, host.rust_bin = root, 'test', 0, root
            with patch.object(runner, 'MeasurementResources', return_value=nullcontext()) as capture, \
                    patch.object(runner.subprocess, 'run', return_value=SimpleNamespace(returncode=0)) as run:
                host.unit('measured', ['/program', '--verify'], root, measurement=True, writable=root)
                self.assertEqual(run.call_args.args[0][-4:], ['/usr/bin/setarch', '--addr-no-randomize', '/program', '--verify'])
                self.assertEqual(capture.call_args.kwargs['executable'], Path('/program'))
                self.assertEqual(capture.call_args.kwargs['benchmark_log'], root / 'measured.stderr')
                host.unit('build', ['/compiler'], root)
                self.assertNotIn('/usr/bin/setarch', run.call_args.args[0])
                self.assertEqual(capture.call_count, 1)

    def test_kernel_work_is_excluded_and_a_restored_broad_mask_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            mask = Path(temporary) / 'mask'
            mask.write_text('00000000,0000ffff\n')
            with patch.object(resources, 'WORKQUEUE_MASK', mask):
                with self.assertRaises(QualificationError):
                    resources.verify_workqueues()
                resources.restrict_workqueues()
                self.assertEqual(resources.workqueue_mask(), 255)
                mask.write_text('ffff')
                with self.assertRaises(QualificationError):
                    resources.verify_workqueues()

    def test_sampling_tracks_only_the_unit_and_retains_process_identity(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            group = root / 'group'
            group.mkdir()
            (group / 'cgroup.procs').write_text('100\n')
            (group / 'cpuset.cpus.effective').write_text('8\n')
            proc = root / 'proc'
            for pid in ['100', '200']:
                task = proc / pid / 'task' / pid
                task.mkdir(parents=True)
                fields = ['0'] * 50
                fields[7], fields[9], fields[19], fields[36] = '45', '2', '123', '8'
                (task / 'stat').write_text(f'{pid} (a name) ' + ' '.join(fields))
                (task / 'schedstat').write_text('10000 100 2\n')
            with patch.object(resources, 'PROC', proc):
                monitor = resources.MeasurementResources(group, root / 'result.json', '8')
                monitor.sample()
                monitor.sample()
                self.assertEqual(list(monitor.tasks), ['100/100/123'])
                self.assertEqual(monitor.tasks['100/100/123']['sampled_cpus'], {'8': 2})
                self.assertEqual(monitor.tasks['100/100/123']['CPU_nanoseconds'], 10000)
                self.assertEqual(monitor.tasks['100/100/123']['minor_faults'], 45)
                self.assertEqual(monitor.tasks['100/100/123']['major_faults'], 2)
                (group / 'cpuset.cpus.effective').write_text('8-15')
                with self.assertRaises(QualificationError):
                    monitor.sample()

    def test_missing_task_evidence_fails_and_retains_diagnostics(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            with self.assertRaises(QualificationError):
                with resources.MeasurementResources(root / 'gone', root / 'result.json', '8'):
                    pass
            record = json.loads((root / 'result.json').read_text())
            self.assertIn('no workload task', record['sampling_error'])
            self.assertEqual(record['last_observed_tasks'], {})

    def test_sampling_failure_is_retained_without_hiding_workload_failure(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            monitor = resources.MeasurementResources(root, root / 'result.json', '8')
            with patch.object(monitor, 'sample', side_effect=RuntimeError('diagnostic failure')):
                with self.assertRaisesRegex(RuntimeError, 'workload failure'):
                    with monitor:
                        monitor.thread.join()
                        raise RuntimeError('workload failure')
            self.assertEqual(json.loads((root / 'result.json').read_text())['sampling_error'], 'diagnostic failure')

    def test_hardware_failure_is_retained_without_hiding_workload_failure(self):
        for workload_fails in (False, True):
            with self.subTest(workload_fails=workload_fails), tempfile.TemporaryDirectory() as temporary, \
                    patch.object(resources, 'snapshot', return_value={}):
                root = Path(temporary)
                monitor = resources.MeasurementResources(root, root / 'result.json', '8')
                self.hardware.finish = lambda: {'sampling_error': 'missing hardware counter'}
                with patch.object(monitor, 'sample'):
                    expected = RuntimeError if workload_fails else QualificationError
                    with self.assertRaisesRegex(expected, 'workload failure' if workload_fails else 'missing hardware'):
                        with monitor:
                            if workload_fails:
                                raise RuntimeError('workload failure')
                record = json.loads((root / 'result.json').read_text())
                self.assertEqual(record['hardware_counters']['sampling_error'], 'missing hardware counter')
                self.assertEqual(record['sampling_error'], 'missing hardware counter')

    def test_cleanup_exception_cannot_replace_observer_start_or_workload_failure(self):
        def cleanup_failure():
            raise OSError('cleanup failure')

        self.hardware.finish = cleanup_failure
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            monitor = resources.MeasurementResources(root, root / 'result.json', '8')
            with patch.object(monitor, 'sample'), self.assertRaisesRegex(RuntimeError, 'workload failure'):
                with monitor:
                    raise RuntimeError('workload failure')
            record = json.loads((root / 'result.json').read_text())
            self.assertEqual(record['sampling_error'], 'hardware counter cleanup failed: cleanup failure')
            def start_failure():
                raise RuntimeError('start failure')
            self.hardware.start = start_failure
            monitor = resources.MeasurementResources(root, root / 'startup.json', '8')
            with self.assertRaisesRegex(RuntimeError, 'start failure'):
                with monitor:
                    self.fail('failed observer must not start the workload')

    def test_benchmark_markers_follow_complete_log_lines_and_keep_observation_times(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            log = root / 'workload.stderr'
            monitor = resources.MeasurementResources(root, root / 'result.json', '8', benchmark_log=log)
            monitor.sample_benchmark_marker()
            self.assertEqual(monitor.benchmark_markers, [])
            log.write_text('Benchmarking q1: Warming up\nBenchmarking q1: Collec')
            with patch.object(resources.time, 'monotonic', return_value=12.5):
                monitor.sample_benchmark_marker()
                monitor.sample_benchmark_marker()
            self.assertEqual(monitor.benchmark_markers, [
                {'observed_monotonic_seconds': 12.5, 'message': 'Benchmarking q1: Warming up'}])
            with log.open('a') as stream:
                stream.write('ting 20 samples\nWarning: an unrelated line\n')
            with patch.object(resources.time, 'monotonic', return_value=13.0):
                monitor.sample_benchmark_marker()
            self.assertEqual(monitor.benchmark_markers[-1], {
                'observed_monotonic_seconds': 13.0, 'message': 'Benchmarking q1: Collecting 20 samples'})
            log.write_text('discarded prefix\n' * 2000 + 'Benchmarking q1: Analyzing\n')
            monitor.sample_benchmark_marker()
            self.assertEqual(monitor.benchmark_markers[-1]['message'], 'Benchmarking q1: Analyzing')

    def test_benchmark_marker_size_and_inventory_are_bounded(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(resources, 'snapshot', return_value={}):
            root = Path(temporary)
            log = root / 'workload.stderr'
            monitor = resources.MeasurementResources(root, root / 'result.json', '8', benchmark_log=log)
            log.write_text('Benchmarking ' + 'x' * 1024 + '\n')
            with self.assertRaisesRegex(QualificationError, 'marker exceeded'):
                monitor.sample_benchmark_marker()
            for index in range(64):
                log.write_text(f'Benchmarking q{index}: Analyzing\n')
                monitor.sample_benchmark_marker()
            log.write_text('Benchmarking q65: Analyzing\n')
            with self.assertRaisesRegex(QualificationError, 'inventory exceeded'):
                monitor.sample_benchmark_marker()


if __name__ == '__main__':
    unittest.main()
