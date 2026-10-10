import contextlib
import io
import importlib
import signal
import os
import struct
from unittest.mock import patch
import json
import tempfile
import unittest
from pathlib import Path


class MeasurementTests(unittest.TestCase):
    def setUp(self):
        try:
            self.m = importlib.import_module('eye_measure')
        except ModuleNotFoundError:
            self.fail('measurement API is not implemented')

    def series(self, n=450):
        a = self.m.Measurement(5)
        for i in range(n):
            a.add(i, 1, i / 90, [0.7, 0.6])
        return a

    def test_layouts(self):
        self.assertEqual(self.m.layout(4, 0x4f21a), (0x152, 0x4f21a))
        self.assertEqual(self.m.layout(5, 0x4f21f), (0x157, 0x4f21f))
        with self.assertRaises(ValueError):
            self.m.layout(5, 0x4f21a)
        with self.assertRaises(ValueError):
            self.m.layout(4, 0x4f21a, '5')

    def test_strict_json(self):
        a = self.m.Measurement(5)
        r = a.add(1, 1, float('nan'), [float('inf'), 0.5])
        obj = json.loads(self.m.encode(r))
        self.assertIsNone(obj['timestamp'])
        self.assertEqual(obj['status'], 'nonfinite_timestamp')
        self.assertIsNone(obj['openness'][0])

    def test_unique_samples(self):
        a = self.m.Measurement(5)
        a.add(0xffffffff, 1, 1, [0.5, 0.5])
        a.add(0, 1, 1, [0.5, 0.5])
        a.add(1, 1, 0.5, [0.5, 0.5])
        self.assertEqual(a.summary(5)['valid'], 1)
        self.assertEqual(a.summary(5)['duplicate_timestamps'], 1)
        self.assertEqual(a.summary(5)['backward_timestamps'], 1)
        self.assertEqual(a.summary(5)['sequence_wraps'], 1)
        self.assertEqual(a.summary(5)['quality'], 'unknown')

    def test_quality_missing_updates(self):
        self.assertEqual(self.series().summary(5)['quality'], 'passed')
        s = self.series(90).summary(5)
        self.assertAlmostEqual(s['valid_rate'], .2)
        self.assertEqual(s['quality'], 'failed')

    def test_unknown_cadence(self):
        self.assertEqual(self.series(10).summary(5)['quality'], 'unknown')

    def test_interrupted(self):
        self.assertEqual(self.series().summary(5, interrupted=True)['quality'], 'failed')
        self.assertEqual(self.series().summary(1)['quality'], 'failed')

    def test_both_eyes_required(self):
        a = self.m.Measurement(5)
        a.add(1, 1, 0, [0.5, float('nan')])
        self.assertEqual(a.summary(5)['valid'], 0)

    def test_summary_validation_unknown(self):
        fixture = Path(__file__).resolve().parents[1] / 'docs/research/fixtures/eyelid-v4'
        result = self.m.evaluate(fixture/'closed.jsonl', fixture/'relaxed.jsonl',
                                 fixture/'wide.jsonl', [fixture/'relaxed.jsonl'], .05, .02, .05, .05)
        self.assertIsNone(result['left']['validation']['leak_rate'])
        self.assertEqual(result['left']['validation']['status'], 'unknown')

    def test_raw_validation_leak(self):
        with tempfile.TemporaryDirectory() as d:
            def write(name, values):
                p = Path(d)/name
                p.write_text('\n'.join(json.dumps({'type':'sample','status':'valid',
                    'openness':[x,x]}) for x in values))
                return p
            c, r, w, v = [write(n,x) for n,x in [('c',[0]),('r',[.5]),('w',[1]),('v',[.5,.7])]]
            out = self.m.evaluate(c,r,w,[v],.1,.1,.05,.05)
            self.assertAlmostEqual(out['left']['validation']['leak_rate'], .5)
            self.assertAlmostEqual(out['left']['validation']['combined']['max'], .8125)

    def test_fixture_replay_cannot_certify_duration(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'samples.jsonl'
            path.write_text('\n'.join(json.dumps({'type':'sample','sequence':i,
                'producer_state':1,'timestamp':i/90,'openness':[.5,.5]}) for i in range(450)))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(self.m.main(['capture','--source',str(path),
                    '--condition','relaxed','--session','fit','--label','run']), 0)
            records = [json.loads(x) for x in output.getvalue().splitlines()]
            self.assertEqual(records[-1]['quality'], 'unknown')
            self.assertTrue(all(r['schema'] == 1 and r['session'] == 'fit' for r in records))

    def test_changed_header_and_size_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'shm'
            with path.open('wb') as f:
                f.write(struct.pack('<II',4,1))
                f.truncate(0x4f21a)
            source = self.m.EyeSource(path, 'auto')
            try:
                with path.open('r+b') as f:
                    f.write(struct.pack('<II',5,1))
                with self.assertRaises(ValueError):
                    source.poll()
                with path.open('r+b') as f:
                    f.write(struct.pack('<II',4,1))
                    f.truncate(8)
                with self.assertRaises(ValueError):
                    source.poll()
            finally:
                source.close()
            self.assertIsNone(source.map)
            self.assertIsNone(source.fd)

    def test_interrupt_capture_closes_and_returns_130(self):
        # Source double replaces hardware only; real signal handler, loop and summary run.
        class Source:
            version = 4
            closed = False
            def __init__(self, *args):
                pass
            def poll(inner):
                self.m.stop_handler(signal.SIGINT, None)
                return 1, 1, 1., [.5,.5]
            def close(inner):
                Source.closed = True
        output = io.StringIO()
        with patch.object(self.m, 'EyeSource', Source), contextlib.redirect_stdout(output):
            status = self.m.main(['capture','--condition','relaxed','--session','fit',
                                  '--label','run','--prepare','0'])
        summary = json.loads(output.getvalue().splitlines()[-1])
        self.assertEqual(status, 130)
        self.assertTrue(Source.closed)
        self.assertTrue(summary['interrupted'])
        self.assertEqual(summary['quality'], 'failed')

    def test_owner_dead_recovery_preserves_future_reads(self):
        # Model native robust-mutex state, replacing only C calls.
        class RobustMutex:
            dead = True
            consistent = False
            poisoned = False
            unlocks = 0
            def pthread_mutex_trylock(inner, ptr):
                if inner.poisoned:
                    return self.m.errno.ENOTRECOVERABLE
                return self.m.errno.EOWNERDEAD if inner.dead else 0
            def pthread_mutex_consistent(inner, ptr):
                inner.consistent = True
                inner.dead = False
                return 0
            def pthread_mutex_unlock(inner, ptr):
                inner.unlocks += 1
                if inner.dead and not inner.consistent:
                    inner.poisoned = True
                return 0
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'shm'
            with path.open('wb') as f:
                f.write(struct.pack('<II',4,1))
                f.truncate(0x4f21a)
            source = self.m.EyeSource(path, 'auto')
            source.libc = RobustMutex()
            try:
                source.map[0x38:0x3c] = struct.pack('<I', 7)
                source.map[source.offset:source.offset+4] = struct.pack('<I', 1)
                source.map[source.offset+5:source.offset+13] = struct.pack('<d', 1.)
                source.map[source.offset+0x79:source.offset+0x81] = struct.pack('<2f', .9, .9)
                with self.assertRaises(self.m.OwnerDeadDiscarded) as caught:
                    source.poll()
                self.assertEqual(caught.exception.sequence, 7)
                self.assertTrue(source.libc.consistent)
                self.assertFalse(source.libc.poisoned)
                self.assertEqual(source.libc.unlocks, 1)
                self.assertEqual(source.map[0x3c:0x40], struct.pack('<I', 1))
                measurement = self.m.Measurement(5)
                measurement.previous_sequence = caught.exception.sequence
                self.assertIsNone(measurement.add(*source.poll()))
                self.assertEqual(measurement.counts['updates'], 0)
                source.map[0x38:0x3c] = struct.pack('<I', 8)
                source.map[source.offset+0x79:source.offset+0x81] = struct.pack('<2f', .5, .5)
                self.assertEqual(measurement.add(*source.poll())['status'], 'valid')
                self.assertEqual(measurement.eyes, [[.5], [.5]])
            finally:
                source.close()

    def test_capture_discards_owner_dead_and_same_partial_sequence(self):
        class Source:
            version = 4
            index = 0
            def __init__(inner, *args):
                pass
            def poll(inner):
                inner.index += 1
                if inner.index == 1:
                    raise self.m.OwnerDeadDiscarded(7)
                if inner.index == 2:
                    return 7, 1, 1., [.9, .9]
                self.m.stop_handler(signal.SIGINT, None)
                return 8, 1, 2., [.5, .5]
            def close(inner):
                pass
        status, records = self.capture_output(Source)
        self.assertEqual(status, 130)
        samples = [r for r in records if r['type'] == 'sample']
        self.assertEqual(len(samples), 1)
        self.assertEqual(samples[0]['sequence'], 8)
        summary = records[-1]
        self.assertEqual(summary['owner_dead_discarded'], 1)
        self.assertEqual(summary['duplicate_sequences'], 1)
        self.assertEqual(summary['polls'], 3)
        self.assertEqual(summary['updates'], 1)
        self.assertEqual(summary['valid'], 1)
        self.assertEqual(summary['left']['max'], .5)

    def test_preparation_owner_dead_excludes_partial_baseline_timestamp(self):
        class Source:
            version = 4
            index = 0
            def __init__(inner, *args):
                pass
            def poll(inner):
                inner.index += 1
                if inner.index == 1:
                    return 6, 1, 1., [.5, .5]
                if inner.index == 2:
                    raise self.m.OwnerDeadDiscarded(7)
                if inner.index == 3:
                    return 7, 1, 100., [.9, .9]
                self.m.stop_handler(signal.SIGINT, None)
                return 8, 1, 2., [.5, .5]
            def close(inner):
                pass
        with patch.object(self.m.time, 'monotonic', side_effect=[0., .1, .2, .3, 1., 1., 1.1, 1.2, 1.3]):
            status, records = self.capture_output(Source, prepare='.5')
        self.assertEqual(status, 130)
        self.assertEqual(records[-1]['valid'], 1)
        self.assertEqual(records[-1]['backward_timestamps'], 0)
        self.assertEqual(records[-1]['owner_dead_discarded'], 0)
        self.assertEqual(records[-1]['left']['max'], .5)

    def capture_output(self, source, prepare='0'):
        output, errors = io.StringIO(), io.StringIO()
        with patch.object(self.m, 'EyeSource', source), contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            code = self.m.main(['capture','--condition','relaxed','--session','fit','--label','run',
                                '--prepare',prepare])
        return code, [json.loads(x) for x in output.getvalue().splitlines()]

    def test_null_replay_roundtrip(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'invalid.jsonl'
            path.write_text(self.m.encode({'type':'sample','sequence':1,'producer_state':1,
                'timestamp':float('nan'),'openness':[float('inf'),.5]})+'\n'+
                json.dumps({'type':'sample','sequence':2,'producer_state':1,
                'timestamp':1.,'openness':[None,.5]}))
            output, errors = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                code = self.m.main(['capture','--source',str(path),'--condition','relaxed',
                                    '--session','fit','--label','run'])
            self.assertEqual(code, 0)
            summary = json.loads(output.getvalue().splitlines()[-1])
            self.assertEqual(summary['nonfinite_timestamps'], 1)
            self.assertEqual(summary['invalid_openness'], 1)

    def test_sequence_restart_quality_unknown(self):
        a = self.series()
        a.add(3,1,6.,[.5,.5])
        summary = a.summary(5)
        self.assertEqual(summary['sequence_restarts'],1)
        self.assertEqual(summary['quality'],'unknown')

    def test_inactive_takes_priority_over_duplicate_timestamp(self):
        a = self.m.Measurement(5)
        a.add(1,1,1.,[.5,.5])
        record = a.add(2,0,1.,[.5,.5])
        self.assertEqual(record['status'],'inactive')
        self.assertEqual(a.summary(5)['inactive'],1)

    def test_missing_estimate_uses_valid_snapshots(self):
        a = self.series(90)
        a.add(90,1,89/90,[.5,.5])
        self.assertAlmostEqual(a.summary(5)['missing_samples_estimate'],360)
        self.assertEqual(a.summary(5)['missing_samples_basis'],'valid_snapshots')

    def test_prepare_stale_record_excluded(self):
        class Source:
            version = 4
            calls = 0
            def __init__(self,*args): pass
            def poll(inner):
                Source.calls += 1
                if Source.calls == 3:
                    self.m.stop_handler(signal.SIGINT,None)
                return 1,1,1.,[.5,.5]
            def close(inner): pass
        code, records = self.capture_output(Source, '.001')
        self.assertEqual(code,130)
        self.assertEqual(records[-1]['valid'],0)

    def test_poll_error_emits_failed_summary_and_closes(self):
        class Source:
            version = 4
            closed = False
            def __init__(self,*args): pass
            def poll(inner): raise ValueError('header changed')
            def close(inner): Source.closed = True
        code, records = self.capture_output(Source)
        self.assertEqual(code,1)
        self.assertEqual(records[-1]['type'],'summary')
        self.assertEqual(records[-1]['quality'],'failed')
        self.assertEqual(records[-1]['error'],'header changed')
        self.assertTrue(Source.closed)

    def test_stop_wait_is_not_busy(self):
        class Source:
            version = 4
            def __init__(self,*args): pass
            def poll(inner):
                self.m.stop_handler(signal.SIGINT,None)
                raise self.m.CaptureStopped()
            def close(inner): pass
        # Missing cancellation API is a failed contract, rather than a test import error.
        self.assertTrue(hasattr(self.m,'CaptureStopped'))
        code, records = self.capture_output(Source)
        self.assertEqual(code,130)
        self.assertEqual(records[-1]['busy'],0)

    def write_dataset(self, directory, name, values, condition=None, session='fit-1', quality='passed', interrupted=False):
        path = Path(directory)/name
        records = [{'type':'sample','status':'valid','openness':[x,x]} for x in values]
        records += [{'type':'summary','condition':condition,'session':session,'label':name,
                     'quality':quality,'interrupted':interrupted}]
        path.write_text('\n'.join(map(json.dumps,records)))
        return path

    def test_evaluate_preserves_source_quality_and_per_file_leaks(self):
        with tempfile.TemporaryDirectory() as d:
            c,r,w = [self.write_dataset(d,n,[x],condition) for n,x,condition in
                     [('c',0,'closed'),('r',.5,'relaxed'),('w',1,'wide')]]
            a = self.write_dataset(d,'a',[.5],'relaxed',session='fit-1')
            b = self.write_dataset(d,'b',[.7],'relaxed',session='fit-2',quality='failed',interrupted=True)
            out = self.m.evaluate(c,r,w,[a,b],.1,.1,.05,.05)
            self.assertFalse(out.get('source_quality_ok',True))
            self.assertEqual(out['validation_sources'][1]['session'],'fit-2')
            self.assertTrue(out['validation_sources'][1]['interrupted'])
            per_file = out['left']['validation']['per_file']
            self.assertEqual([x['leak_rate'] for x in per_file],[0,1])
            self.assertEqual(out['left']['validation']['status'],'unqualified')

    def test_evaluate_rejects_known_condition_mismatch(self):
        with tempfile.TemporaryDirectory() as d:
            c = self.write_dataset(d,'c',[0],'wide')
            r = self.write_dataset(d,'r',[.5],'relaxed')
            w = self.write_dataset(d,'w',[1],'wide')
            with self.assertRaises(ValueError):
                self.m.evaluate(c,r,w,[r],.1,.1,.05,.05)

    def test_legacy_quality_and_unknown_reasons(self):
        fixture = Path(__file__).resolve().parents[1]/'docs/research/fixtures/eyelid-v4'
        out = self.m.evaluate(fixture/'closed.jsonl',fixture/'relaxed.jsonl',fixture/'wide.jsonl',
                             [fixture/'relaxed.jsonl'],.05,.02,.05,.05)
        self.assertIsInstance(out['calibration_sources'][0],dict)
        self.assertEqual(out['calibration_sources'][0]['quality'],'unknown')
        self.assertEqual(out['calibration_sources'][0]['quality_reason'],'legacy_quality_missing')
        self.assertIn('no_validation_raw',out['left']['validation']['reasons'])
        self.assertFalse(out['left']['validation']['eye_wide_available'])
        self.assertFalse(out['left']['validation']['combined_available'])

    def test_wide_unusable_keeps_base_distribution(self):
        with tempfile.TemporaryDirectory() as d:
            c,r,w,v = [self.write_dataset(d,n,x,k) for n,x,k in
                       [('c',[0],'closed'),('r',[.5],'relaxed'),('w',[.55],'wide'),('v',[.5,.7],'relaxed')]]
            out = self.m.evaluate(c,r,w,[v],.1,.1,.05,.05)['left']
            self.assertFalse(out['wide_usable'])
            self.assertEqual(out['validation']['combined']['count'],2)
            self.assertEqual(out['validation']['combined']['max'],.75)
            self.assertIsNone(out['validation']['leak_rate'])
            self.assertIn('wide_unusable',out['validation']['reasons'])

    def test_evaluate_cli_candidate_numbers_required(self):
        options = ['--margin-left','.1','--margin-right','.1','--min-base-width','.05','--min-wide-width','.05']
        required = ['--closed','c','--relaxed','r','--wide','w','--validation-relaxed','v']
        for i in range(0,8,2):
            with self.subTest(option=options[i]), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    self.m.main(['evaluate']+required+options[:i]+options[i+2:])
                self.assertEqual(error.exception.code,2)

    def test_replay_retains_original_context_and_interruption(self):
        with tempfile.TemporaryDirectory() as d:
            path = self.write_dataset(d,'relaxed.jsonl',[],'relaxed',session='original-fit',
                                      quality='failed',interrupted=True)
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.assertEqual(self.m.main(['capture','--source',str(path),'--condition','relaxed',
                    '--session','replay-fit','--label','run']),0)
            records = [json.loads(x) for x in output.getvalue().splitlines()]
            self.assertEqual(records[0]['replay_source']['session'],'original-fit')
            self.assertEqual(records[-1]['quality'],'failed')
            self.assertTrue(records[-1]['interrupted'])

    def test_missing_summary_is_unqualified(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'unfinished.jsonl'
            path.write_text(json.dumps({'type':'sample','status':'valid','openness':[.5,.5]}))
            data = self.m.dataset(path,'relaxed')
            self.assertFalse(data['source']['source_quality_ok'])
            self.assertEqual(data['source']['quality_reason'],'summary_missing')

    def test_unknown_capture_reason(self):
        summary = self.series(10).summary(5)
        self.assertEqual(summary.get('quality_reason'),'insufficient_timestamp_differences')
        a = self.series()
        a.add(3,1,6.,[.5,.5])
        self.assertEqual(a.summary(5).get('quality_reason'),'producer_epoch_changed')

    def test_real_poll_stop_while_mutex_busy(self):
        class BusyMutex:
            def pthread_mutex_trylock(inner, ptr):
                self.m.stop_handler(signal.SIGINT,None)
                return self.m.errno.EBUSY
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'shm'
            with path.open('wb') as f:
                f.write(struct.pack('<II',4,1))
                f.truncate(0x4f21a)
            source = self.m.EyeSource(path,'auto')
            source.libc = BusyMutex()
            try:
                with self.assertRaises(self.m.CaptureStopped):
                    source.poll()
            finally:
                source.close()
                self.m.STOP = None

    def test_mixed_validation_reports_partial_aggregate(self):
        with tempfile.TemporaryDirectory() as d:
            c,r,w,v = [self.write_dataset(d,n,x,k) for n,x,k in
                       [('c',[0],'closed'),('r',[.5],'relaxed'),('w',[1],'wide'),('v',[.7],'relaxed')]]
            empty = self.write_dataset(d,'empty',[],'relaxed')
            out = self.m.evaluate(c,r,w,[v,empty],.1,.1,.05,.05)['left']['validation']
            self.assertTrue(out.get('aggregate_partial',False))
            self.assertFalse(out['eye_wide_available'])
            self.assertFalse(out['combined_available'])
            self.assertEqual(out['combined']['count'],1)
            self.assertEqual(out['status'],'unknown')

    def test_calibration_overlap_path_and_identity(self):
        with tempfile.TemporaryDirectory() as d:
            c,r,w = [self.write_dataset(d,n,[x],k) for n,x,k in
                     [('c',0,'closed'),('r',.5,'relaxed'),('w',1,'wide')]]
            # Copy identity and symlink identity are independent detection paths.
            copy = Path(d)/'copy'
            copy.write_text(r.read_text())
            out = self.m.evaluate(c,r,w,[copy],.1,.1,.05,.05)
            self.assertTrue(out['validation_overlaps_calibration'])
            self.assertEqual(out['left']['validation']['status'],'unqualified')
            raw = Path(d)/'raw'
            raw.write_text(json.dumps({'type':'sample','status':'valid','openness':[.5,.5]}))
            alias = Path(d)/'alias'
            alias.symlink_to(raw)
            out = self.m.evaluate(c,raw,w,[alias],.1,.1,.05,.05)
            self.assertIsNone(out['validation_sources'][0]['session'])
            self.assertIsNone(out['validation_sources'][0]['label'])
            self.assertTrue(out['validation_overlaps_calibration'])
            self.assertTrue(out['left']['validation']['per_file'][0]['validation_overlaps_calibration'])

    def test_long_replay_has_no_invented_coverage(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'long.jsonl'
            path.write_text('\n'.join(json.dumps({'type':'sample','sequence':i,'producer_state':1,
                'timestamp':i/90,'openness':[.5,.5]}) for i in range(5400)))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                self.m.main(['capture','--source',str(path),'--condition','relaxed','--session','fit','--label','run'])
            replay_records = [json.loads(x) for x in output.getvalue().splitlines()]
            self.assertIsNone(replay_records[0]['duration'])
            summary = replay_records[-1]
            self.assertIsNone(summary['duration'])
            self.assertIsNone(summary['elapsed'])
            self.assertIsNone(summary['expected_samples'])
            self.assertIsNone(summary['valid_rate'])
            self.assertIsNone(summary['missing_samples_estimate'])
            self.assertIsNone(summary['timestamp_resolution_ok'])
            self.assertIsNone(summary['coverage_comparison_tolerance'])
            self.assertIsNone(summary['timestamp_quantum'])
            self.assertIsNone(summary['observed_step_grain'])
            self.assertIsNone(summary['coverage_rate_uncertainty'])

    def test_failed_replay_preserves_reason(self):
        with tempfile.TemporaryDirectory() as d:
            for interrupted, reason in [(True,'interrupted'),(False,'source_quality_failed')]:
                path = self.write_dataset(d,'failed.jsonl',[],'relaxed',quality='failed',interrupted=interrupted)
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    self.m.main(['capture','--source',str(path),'--condition','relaxed','--session','fit','--label','run'])
                summary = json.loads(output.getvalue().splitlines()[-1])
                self.assertEqual(summary['quality_reason'],reason)

    def test_legacy_condition_exact_label_inference(self):
        fixture = Path(__file__).resolve().parents[1]/'docs/research/fixtures/eyelid-v4'
        with self.assertRaises(ValueError):
            self.m.dataset(fixture/'wide.jsonl','closed')
        self.assertTrue(self.m.dataset(fixture/'wide.jsonl','wide')['source']['condition_verified'])
        with tempfile.TemporaryDirectory() as d:
            path = self.write_dataset(d,'wide-something',[],None)
            self.assertFalse(self.m.dataset(path,'closed')['source']['condition_verified'])

    def test_nonobject_jsonl_reports_line_and_failed_summary(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d)/'bad.jsonl'
            path.write_text('{}\n[1,2]\n')
            output, errors = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                try:
                    code = self.m.main(['capture','--source',str(path),'--condition','relaxed','--session','fit','--label','run'])
                except AttributeError as error:
                    self.fail(f'nonobject JSONL must produce a failed summary: {error}')
            self.assertEqual(code,1)
            summary = json.loads(output.getvalue().splitlines()[-1])
            self.assertEqual(summary['quality'],'failed')
            self.assertIn(':2: expected JSON object',summary['error'])

    def test_quality_exact_eighty_percent_boundary(self):
        for origin in (0.,4e6,1.8e9):
            for valid, quality in [(360,'passed'),(359,'failed')]:
                with self.subTest(origin=origin,valid=valid):
                    a = self.m.Measurement(5)
                    for i in range(450):
                        a.add(i,1,origin+i/90,[.7,.6] if i < valid else [None,.6])
                    self.assertEqual(a.summary(5)['quality'],quality)

    def test_coarse_timestamp_resolution_cannot_qualify(self):
        a = self.m.Measurement(60)
        for i in range(5400):
            a.add(i,1,1e16+i/90,[.7,.6])
        summary = a.summary(60)
        self.assertEqual(summary['quality'],'unknown')
        self.assertEqual(summary['quality_reason'],'timestamp_resolution_insufficient')
        self.assertEqual(a.summary(60,interrupted=True)['quality'],'failed')

    def test_identity_across_different_conditions_is_independent(self):
        with tempfile.TemporaryDirectory() as d:
            c = self.write_dataset(d,'take-1',[0],'closed')
            r = self.write_dataset(d,'r',[.5],'relaxed')
            w = self.write_dataset(d,'w',[1],'wide')
            v = Path(d)/'v'
            records = [{'type':'sample','status':'valid','openness':[.7,.7]},
                       {'type':'summary','session':'fit-1','label':'take-1','condition':'relaxed',
                        'quality':'passed','interrupted':False}]
            v.write_text('\n'.join(map(json.dumps,records)))
            out = self.m.evaluate(c,r,w,[v],.1,.1,.05,.05)
            self.assertFalse(out['validation_overlaps_calibration'])
            self.assertEqual(out['left']['validation']['status'],'known')

    def test_hardlink_calibration_overlap_without_metadata(self):
        with tempfile.TemporaryDirectory() as d:
            c = self.write_dataset(d,'c',[0],'closed')
            w = self.write_dataset(d,'w',[1],'wide')
            r = Path(d)/'raw'
            r.write_text(json.dumps({'type':'sample','status':'valid','openness':[.5,.5]}))
            alias = Path(d)/'hardlink'
            os.link(r,alias)
            out = self.m.evaluate(c,r,w,[alias],.1,.1,.05,.05)
            self.assertTrue(out['validation_overlaps_calibration'])
            self.assertEqual(out['calibration_sources'][1]['file_identity'],out['validation_sources'][0]['file_identity'])

    def test_duplicate_validation_excluded_from_aggregate(self):
        with tempfile.TemporaryDirectory() as d:
            c,r,w,v,a = [self.write_dataset(d,n,x,k) for n,x,k in
                [('c',[0],'closed'),('r',[.5],'relaxed'),('w',[1],'wide'),
                 ('v',[.7],'relaxed'),('a',[.5],'relaxed')]]
            alias = Path(d)/'hardlink'
            os.link(v,alias)
            for duplicate in (v,alias):
                with self.subTest(duplicate=duplicate):
                    out = self.m.evaluate(c,r,w,[v,duplicate,a],.1,.1,.05,.05)
                    self.assertTrue(out.get('duplicate_validation_input',False))
                    self.assertTrue(out['validation_sources'][1]['duplicate_validation_input'])
                    validation = out['left']['validation']
                    self.assertEqual(validation['raw_count'],2)
                    self.assertEqual(validation['leak_rate'],.5)
                    self.assertEqual(validation['status'],'unqualified')
                    self.assertIn('duplicate_validation_input',validation['reasons'])

    def test_rounded_millisecond_clock_is_ambiguous(self):
        a = self.m.Measurement(5)
        for i in range(421):
            a.add(i,1,round(i*11.9)/1000,[.7,.6] if i < 334 else [None,.6])
        summary = a.summary(5)
        self.assertEqual(summary['quality'],'unknown')
        self.assertEqual(summary['quality_reason'],'timestamp_resolution_insufficient')

    def test_long_epoch_capture_obvious_coverage(self):
        for valid, quality in [(26730,'passed'),(2700,'failed')]:
            with self.subTest(valid=valid):
                a = self.m.Measurement(300)
                for i in range(27000):
                    a.add(i,1,1.8e9+i/90,[.7,.6] if i < valid else [None,.6])
                self.assertEqual(a.summary(300)['quality'],quality)

    def test_correction_does_not_double_quantization_error(self):
        a = self.m.Measurement(60.001)
        for i in range(5401):
            a.add(i,1,1.8e9+i/90,[.7,.6] if i < 4320 else [None,.6])
        self.assertEqual(a.summary(60.001)['quality'],'failed')

    def test_missing_period_has_no_resolution_diagnosis(self):
        a = self.series(10)
        self.assertIsNone(a.summary(5)['timestamp_resolution_ok'])
        b = self.series()
        b.add(3,1,6.,[.7,.6])
        self.assertIsNone(b.summary(5)['timestamp_resolution_ok'])

    def test_single_skipped_update_preserves_quality(self):
        for origin in (0.,4e6,1.8e9):
            with self.subTest(origin=origin):
                a = self.m.Measurement(5)
                for i in range(451):
                    if i != 200:
                        a.add(i,1,origin+i/90,[.7,.6])
                summary = a.summary(5)
                self.assertEqual(summary['valid'],450)
                self.assertEqual(summary['quality'],'passed')
                self.assertEqual(summary['observed_step_grain'],0)

    def test_stop_and_resume_preserves_low_coverage_failure(self):
        a = self.m.Measurement(5)
        for i in list(range(90))+list(range(360,450)):
            a.add(i,1,i/90,[.7,.6])
        summary = a.summary(5)
        self.assertEqual(summary['valid'],180)
        self.assertEqual(summary['quality'],'failed')
        self.assertEqual(summary['quality_reason'],'valid_coverage_below_threshold')
        self.assertAlmostEqual(summary['valid_rate'],.4)

    def test_short_clock_steps_preserve_ambiguity(self):
        a = self.m.Measurement(5)
        grain = 3/512
        for i in range(451):
            a.add(i,1,(i*512//270)*grain,[.7,.6] if i < 345 else [None,.6])
        summary = a.summary(5)
        self.assertEqual(summary['quality'],'unknown')
        self.assertEqual(summary['quality_reason'],'timestamp_resolution_insufficient')
        self.assertEqual(summary['observed_step_grain'],3/512)

if __name__ == '__main__':
    unittest.main()
