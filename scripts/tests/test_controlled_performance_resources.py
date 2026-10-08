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
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import controlled_performance_resources as resources
from performance_qualification import QualificationError


class ResourceEvidenceTest(unittest.TestCase):
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
                fields[19], fields[36] = '123', '8'
                (task / 'stat').write_text(f'{pid} (a name) ' + ' '.join(fields))
                (task / 'schedstat').write_text('10000 100 2\n')
            with patch.object(resources, 'PROC', proc):
                monitor = resources.MeasurementResources(group, root / 'result.json', '8')
                monitor.sample()
                monitor.sample()
                self.assertEqual(list(monitor.tasks), ['100/100/123'])
                self.assertEqual(monitor.tasks['100/100/123']['sampled_cpus'], {'8': 2})
                self.assertEqual(monitor.tasks['100/100/123']['CPU_nanoseconds'], 10000)
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


if __name__ == '__main__':
    unittest.main()
