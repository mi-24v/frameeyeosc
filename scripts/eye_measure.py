#!/usr/bin/env python3
"""Local raw eye capture and explicit, offline calibration candidate evaluation."""
import argparse
from collections import Counter
import ctypes
import errno
import json
import math
import mmap
import os
import signal
import statistics
import struct
import sys
import time

LAYOUTS = {4: (0x152, 0x4F21A), 5: (0x157, 0x4F21F)}
STOP = None


def layout(version, size, requested='auto'):
    if version not in LAYOUTS or (requested != 'auto' and int(requested) != version):
        raise ValueError('unsupported or mismatched shared-memory version')
    if size != LAYOUTS[version][1]:
        raise ValueError('shared-memory size does not match ABI')
    return LAYOUTS[version]


def sanitize(value):
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if isinstance(value, dict):
        return {k: sanitize(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [sanitize(v) for v in value]
    return value


def encode(record):
    return json.dumps(sanitize(record), allow_nan=False)


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def stats(values):
    return {'count': len(values), 'min': min(values) if values else None,
            'max': max(values) if values else None,
            'median': statistics.median(values) if values else None}


class Measurement:
    def __init__(self, duration):
        self.duration = duration
        self.counts = Counter()
        self.seen = set()
        self.previous_time = None
        self.previous_sequence = None
        self.deltas = []
        self.eyes = [[], []]

    def add(self, sequence, state, timestamp, openness):
        self.counts['polls'] += 1
        if sequence == self.previous_sequence:
            self.counts['duplicate_sequences'] += 1
            return None
        if self.previous_sequence is not None and sequence < self.previous_sequence:
            if self.previous_sequence > 0xF0000000 and sequence < 0x10000000:
                self.counts['sequence_wraps'] += 1
            else:
                self.counts['sequence_restarts'] += 1
        self.previous_sequence = sequence
        self.counts['updates'] += 1
        status = 'valid'
        if not finite(timestamp):
            status = 'nonfinite_timestamp'
        elif timestamp in self.seen:
            status = 'duplicate_timestamp'
        else:
            self.seen.add(timestamp)
            if self.previous_time is not None:
                delta = timestamp - self.previous_time
                if delta < 0:
                    self.counts['backward_timestamps'] += 1
                    status = 'backward_timestamp'
                elif delta > 0:
                    self.deltas.append(delta)
            self.previous_time = timestamp
        if state != 1:
            status = 'inactive'
        elif status == 'valid' and (not isinstance(openness, (list, tuple)) or len(openness) != 2
                                   or not all(finite(x) and 0 <= x <= 1 for x in openness)):
            status = 'invalid_openness'
        key = {'nonfinite_timestamp': 'nonfinite_timestamps',
               'duplicate_timestamp': 'duplicate_timestamps'}.get(status, status)
        if status != 'backward_timestamp':
            self.counts[key] += 1
        if status == 'valid':
            for i in range(2):
                self.eyes[i].append(openness[i])
        return {'type': 'sample', 'sequence': sequence, 'producer_state': state,
                'timestamp': timestamp, 'openness': openness, 'status': status}

    def summary(self, elapsed, interrupted=False):
        keys = ('polls', 'updates', 'valid', 'inactive', 'invalid_openness', 'busy',
                'duplicate_sequences', 'duplicate_timestamps', 'backward_timestamps',
                'nonfinite_timestamps', 'sequence_wraps', 'sequence_restarts', 'owner_dead_discarded')
        period = None
        if len(self.deltas) >= 10 and not (self.counts['backward_timestamps'] or self.counts['sequence_restarts']):
            period = statistics.median(self.deltas)
        expected = self.duration / period if period else None
        raw_rate = self.counts['valid'] / expected if expected else None
        rate = min(1, raw_rate) if raw_rate is not None else None
        float_quantum = max((math.ulp(t) for t in self.seen), default=0.)
        # Ignore near-identical delta variants caused by floating-point subtraction.
        # Only separations beyond endpoint representation noise reveal observed grain.
        # Multiples of the normal period and stop/resume gaps describe coverage loss,
        # not timestamp quantization. Preserve shorter steps that reveal clock grain.
        steps = sorted({d for d in self.deltas if d < 1.5*period}) if period else []
        grain = min((b-a for a, b in zip(steps, steps[1:]) if b-a > 2*float_quantum), default=0.)
        quantum = max(float_quantum, grain)
        uncertainty = 1e-12 + raw_rate * quantum / period if period else None
        resolution_ok = None if expected is None else uncertainty < .5 / expected
        if period is None:
            quality = 'unknown'
            reason = ('producer_epoch_changed' if self.counts['backward_timestamps'] or self.counts['sequence_restarts']
                      else 'insufficient_timestamp_differences')
        elif raw_rate - uncertainty >= .8:
            quality, reason = 'passed', None
        elif raw_rate + uncertainty < .8:
            quality, reason = 'failed', 'valid_coverage_below_threshold'
        elif resolution_ok:
            # Only the narrow representation-error band may resolve an exact tie.
            quality, reason = 'passed', None
        else:
            quality, reason = 'unknown', 'timestamp_resolution_insufficient'
        if interrupted or elapsed < self.duration or self.duration < 5:
            quality = 'failed'
            reason = 'interrupted' if interrupted else 'duration_too_short' if self.duration < 5 else 'measurement_incomplete'
        return {'type': 'summary', **{k: self.counts[k] for k in keys},
                'elapsed': elapsed, 'duration': self.duration, 'interrupted': interrupted,
                'observed_period': period, 'expected_samples': expected, 'valid_rate': rate,
                'timestamp_quantum': quantum, 'observed_step_grain': grain, 'timestamp_resolution_ok': resolution_ok,
                'coverage_rate_uncertainty': uncertainty,
                'coverage_comparison_tolerance': uncertainty if resolution_ok else None,
                'missing_samples_estimate': max(0, expected-self.counts['valid']) if expected else None,
                'missing_samples_basis': 'valid_snapshots',
                'quality': quality, 'quality_reason': reason, 'left': stats(self.eyes[0]), 'right': stats(self.eyes[1]),
                'cadence_limitation': 'Observed timestamp cadence only; skipped updates, mixed producer periods, or consistently slow publishing can bias the inferred producer rate.'}


class LockBusy(Exception):
    pass


class CaptureStopped(Exception):
    pass


class OwnerDeadDiscarded(Exception):
    def __init__(self, sequence):
        self.sequence = sequence
        super().__init__('potentially partial owner-dead record discarded')


class EyeSource:
    def __init__(self, path, requested):
        self.fd = os.open(path, os.O_RDWR)
        self.map = None
        self.view = None
        try:
            self.size = os.fstat(self.fd).st_size
            header = os.pread(self.fd, 8, 0)
            if len(header) != 8:
                raise ValueError('truncated shared-memory header')
            self.version, initialized = struct.unpack('<II', header)
            self.offset, _ = layout(self.version, self.size, requested)
            if initialized != 1:
                raise ValueError('shared memory is not initialized')
            self.map = mmap.mmap(self.fd, self.size, flags=mmap.MAP_SHARED,
                                 prot=mmap.PROT_READ | mmap.PROT_WRITE)
            self.view = (ctypes.c_ubyte * self.size).from_buffer(self.map)
            self.base = ctypes.addressof(self.view)
            self.libc = ctypes.CDLL('libc.so.6', use_errno=True)
            for name in ('pthread_mutex_trylock', 'pthread_mutex_unlock', 'pthread_mutex_consistent'):
                f = getattr(self.libc, name)
                f.argtypes = [ctypes.c_void_p]
                f.restype = ctypes.c_int
            self.check()
        except BaseException:
            self.close()
            raise

    def check(self):
        if os.fstat(self.fd).st_size != self.size:
            raise ValueError('shared-memory size changed during capture')
        if os.pread(self.fd, 8, 0) != struct.pack('<II', self.version, 1):
            raise ValueError('shared-memory header changed during capture')

    def poll(self):
        self.check()
        mutex = ctypes.c_void_p(self.base + 8)
        request = struct.pack('<I', 1)
        deadline = time.monotonic() + .5
        while True:
            code = self.libc.pthread_mutex_trylock(mutex)
            if code == 0:
                break
            if code == errno.EOWNERDEAD:
                # Match the reader ABI contract and avoid poisoning the shared mutex.
                recovered = self.libc.pthread_mutex_consistent(mutex)
                if recovered:
                    self.libc.pthread_mutex_unlock(mutex)
                    raise OSError(recovered, 'pthread_mutex_consistent failed')
                try:
                    sequence = struct.unpack_from('<I', self.map, 0x38)[0]
                    self.map[0x3C:0x40] = request
                finally:
                    unlocked = self.libc.pthread_mutex_unlock(mutex)
                    if unlocked:
                        raise OSError(unlocked, 'pthread_mutex_unlock failed')
                self.check()
                raise OwnerDeadDiscarded(sequence)
            if code != errno.EBUSY:
                raise OSError(code, 'pthread_mutex_trylock failed')
            if STOP is not None:
                raise CaptureStopped()
            if time.monotonic() >= deadline:
                raise LockBusy()
            time.sleep(.001)
        try:
            raw = self.map[:self.offset + 0xEBC]
            self.map[0x3C:0x40] = request
        finally:
            code = self.libc.pthread_mutex_unlock(mutex)
            if code:
                raise OSError(code, 'pthread_mutex_unlock failed')
        self.check()
        if raw[:8] != struct.pack('<II', self.version, 1):
            raise ValueError('snapshot header does not match ABI')
        return (struct.unpack_from('<I', raw, 0x38)[0],
                struct.unpack_from('<I', raw, self.offset)[0],
                struct.unpack_from('<d', raw, self.offset+5)[0],
                list(struct.unpack_from('<2f', raw, self.offset+0x79)))

    def close(self):
        self.view = None
        if self.map is not None:
            self.map.close()
            self.map = None
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


def read_records(path):
    records = []
    with open(path) as f:
        for number, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                record = json.loads(line, parse_constant=lambda x: (_ for _ in ()).throw(ValueError('nonfinite JSON')))
            except ValueError as error:
                raise ValueError(f'{path}:{number}: {error}') from error
            if not isinstance(record, dict):
                raise ValueError(f'{path}:{number}: expected JSON object')
            records.append(record)
    return records


def dataset(path, expected_condition=None):
    records = read_records(path)
    summaries = [r for r in records if r.get('type') == 'summary']
    summary = summaries[-1] if summaries else None
    file_stat = os.stat(path)
    info = {'path': str(path), 'condition': None, 'session': None, 'label': None,
            'file_identity': {'device': file_stat.st_dev, 'inode': file_stat.st_ino}}
    conditions = ('closed', 'relaxed', 'wide', 'left-wink', 'right-wink')
    info['condition_verified'] = False
    info['condition_source'] = None
    for record in records:
        condition = record.get('condition')
        origin = 'condition'
        if condition is None and record.get('label') in conditions:
            condition, origin = record['label'], 'legacy_label'
        if expected_condition and condition not in (None, expected_condition):
            raise ValueError(f'{path}: expected condition {expected_condition}, got {condition}')
        if condition is not None:
            info['condition'] = condition
            info['condition_verified'] = expected_condition is not None and condition == expected_condition
            info['condition_source'] = origin
        for key in ('condition', 'session', 'label'):
            if record.get(key) is not None:
                info[key] = record[key]
    info['summary_present'] = summary is not None
    info['quality'] = summary.get('quality', 'unknown') if summary else 'unknown'
    info['interrupted'] = summary.get('interrupted', False) if summary else None
    info['error'] = summary.get('error') if summary else None
    info['quality_reason'] = (summary.get('quality_reason') if summary else None)
    if not summary:
        info['quality_reason'] = 'summary_missing'
    elif 'quality' not in summary:
        info['quality_reason'] = 'legacy_quality_missing'
    info['source_quality_ok'] = (info['quality'] == 'passed' and info['interrupted'] is False
                                 and not info['error'])
    samples = [r['openness'] for r in records if r.get('type') == 'sample' and r.get('status') == 'valid'
               and isinstance(r.get('openness'), (list, tuple)) and len(r['openness']) == 2
               and all(finite(x) and 0 <= x <= 1 for x in r['openness'])]
    return {'samples': samples, 'summary': summary, 'source': info}


def validation_stats(values, base_ok, wide_ok, c, r, start, w, source_ok, overlap=False, duplicate=False):
    reasons = []
    if not values:
        reasons.append('no_validation_raw')
    if not base_ok:
        reasons.append('base_unusable')
    if not wide_ok:
        reasons.append('wide_unusable')
    if not source_ok:
        reasons.append('source_quality_not_passed')
    if overlap:
        reasons.append('validation_overlaps_calibration')
    if duplicate:
        reasons.append('duplicate_validation_input')
    combined, extra_values = [], []
    if base_ok:
        for x in values:
            base = max(0, min(1, (x-c)/(r-c)))
            extra = max(0, min(1, (x-start)/(w-start))) if wide_ok else 0
            extra_values.append(extra)
            combined.append(min(1, .75*base + .25*extra))
    available = bool(values) and base_ok and wide_ok
    return {'status': ('known' if source_ok and not overlap and not duplicate else 'unqualified') if available else 'unknown',
            'reasons': reasons, 'raw_count': len(values), 'source_quality_ok': source_ok,
            'validation_overlaps_calibration': overlap, 'duplicate_validation_input': duplicate,
            'eye_wide_available': available,
            'combined_available': bool(values) and base_ok, 'wide_disabled_reason': None if wide_ok else 'wide_unusable',
            'leak_rate': sum(x > 0 for x in extra_values)/len(extra_values) if available else None,
            'eye_wide': stats(extra_values), 'combined': stats(combined)}


def evaluate(closed, relaxed, wide, validation, margin_left, margin_right, min_base_width, min_wide_width):
    candidates = (margin_left, margin_right, min_base_width, min_wide_width)
    if not all(finite(x) and x >= 0 for x in candidates) or min_base_width <= 0 or min_wide_width <= 0:
        raise ValueError('margins must be nonnegative and minimum widths positive')
    calibration = [dataset(p, condition) for p, condition in zip((closed, relaxed, wide), ('closed','relaxed','wide'))]
    validations = [dataset(p, 'relaxed') for p in validation]
    def same_source(source, other):
        same_file = source['file_identity'] == other['file_identity']
        same_identity = (source['session'] is not None and source['label'] is not None
                         and source['session'] == other['session'] and source['label'] == other['label']
                         and source['condition'] == other['condition'])
        return same_file or same_identity
    unique_validations = []
    for data in validations:
        source = data['source']
        source['validation_overlaps_calibration'] = any(same_source(source, d['source']) for d in calibration)
        duplicate = next((d for d in unique_validations if same_source(source, d['source'])), None)
        source['duplicate_validation_input'] = duplicate is not None
        source['included_in_aggregate'] = duplicate is None
        source['duplicate_of'] = duplicate['source']['path'] if duplicate else None
        if duplicate is None:
            unique_validations.append(data)
    any_duplicate = any(d['source']['duplicate_validation_input'] for d in validations)
    any_overlap = any(d['source']['validation_overlaps_calibration'] for d in validations)
    calibration_ok = all(d['source']['source_quality_ok'] for d in calibration)
    sources_ok = calibration_ok and all(d['source']['source_quality_ok'] for d in validations)
    result = {'schema': 1, 'type': 'evaluation',
              'calibration_sources': [d['source'] for d in calibration],
              'validation_sources': [d['source'] for d in validations],
              'source_quality_ok': sources_ok, 'validation_overlaps_calibration': any_overlap,
              'duplicate_validation_input': any_duplicate,
              'candidate': dict(zip(('margin_left','margin_right','min_base_width','min_wide_width'), candidates))}
    for i, side in enumerate(('left', 'right')):
        def representative(data, key):
            raw, summary = data['samples'], data['summary']
            if raw:
                return stats([x[i] for x in raw])[key]
            value = summary[side][key] if summary else None
            if not finite(value) or not 0 <= value <= 1:
                raise ValueError('missing finite calibration representative in [0, 1]')
            return value
        c, r, w = [representative(x, 'median') for x in calibration]
        rmax = representative(calibration[1], 'max')
        start = rmax + candidates[i]
        base_ok, wide_ok = r-c >= min_base_width, w-start >= min_wide_width
        raw = [x[i] for data in unique_validations for x in data['samples']]
        overall = validation_stats(raw, base_ok, wide_ok, c,r,start,w,sources_ok,any_overlap,any_duplicate)
        missing_raw = any(not d['samples'] for d in unique_validations)
        overall['aggregate_partial'] = bool(raw) and missing_raw
        if missing_raw:
            overall['eye_wide_available'] = False
            overall['combined_available'] = False
            overall['status'] = 'unknown'
            overall['leak_rate'] = None
            if 'no_validation_raw' not in overall['reasons']:
                overall['reasons'].append('no_validation_raw')
        overall['per_file'] = [
            {'source': d['source'], **validation_stats([x[i] for x in d['samples']], base_ok,wide_ok,
                                                      c,r,start,w,calibration_ok and d['source']['source_quality_ok'],
                                                      d['source']['validation_overlaps_calibration'],
                                                      d['source']['duplicate_validation_input'])}
            for d in validations]
        result[side] = {'closed': c, 'relaxed': r, 'wide': w, 'relaxed_max': rmax,
                        'wide_start': start, 'base_usable': base_ok, 'wide_usable': wide_ok,
                        'validation': overall}
    return result


def stop_handler(signum, frame):
    global STOP
    STOP = signum


def capture(args):
    global STOP
    STOP = None
    context = {'schema': 1, 'session': args.session, 'condition': args.condition, 'label': args.label}
    def emit(record):
        print(encode({**record, **context}), flush=True)
    measurement = Measurement(args.duration)
    old = {s: signal.signal(s, stop_handler) for s in (signal.SIGINT, signal.SIGTERM)}
    source, start, error, replay_source = None, None, None, None
    fixture = str(args.source).endswith('.jsonl')
    try:
        if fixture:
            data = dataset(args.source, args.condition)
            replay_source = data['source']
            emit({'type': 'metadata', 'source': str(args.source), 'duration': None,
                  'fixture': True, 'replay_source': replay_source})
            for r in read_records(args.source):
                if STOP is not None:
                    break
                if r.get('type') == 'sample':
                    record = measurement.add(r['sequence'], r['producer_state'], r['timestamp'], r['openness'])
                    if record:
                        emit(record)
        else:
            source = EyeSource(args.source, args.shm_version)
            emit({'type': 'metadata', 'source': str(args.source), 'version': source.version,
                  'duration': args.duration, 'prepare': args.prepare})
            prepare_end = time.monotonic() + args.prepare
            baseline = None
            while STOP is None and time.monotonic() < prepare_end:
                try:
                    candidate = source.poll()
                    baseline = candidate if candidate[0] != measurement.previous_sequence else None
                except OwnerDeadDiscarded as discarded:
                    baseline = None
                    measurement.previous_sequence = discarded.sequence
                except LockBusy:
                    pass
                time.sleep(.006)
            if baseline is not None:
                sequence, _, timestamp, _ = baseline
                measurement.previous_sequence = sequence
                if finite(timestamp):
                    measurement.previous_time = timestamp
                    measurement.seen.add(timestamp)
            start = time.monotonic()
            while STOP is None and time.monotonic()-start < args.duration:
                try:
                    record = measurement.add(*source.poll())
                    if record:
                        record['t'] = time.monotonic()-start
                        emit(record)
                except OwnerDeadDiscarded as discarded:
                    measurement.previous_sequence = discarded.sequence
                    measurement.counts['owner_dead_discarded'] += 1
                    measurement.counts['polls'] += 1
                except LockBusy:
                    measurement.counts['busy'] += 1
                    measurement.counts['polls'] += 1
                time.sleep(.006)
    except CaptureStopped:
        pass
    except (OSError, ValueError, KeyError, TypeError) as exc:
        error = str(exc)
    finally:
        try:
            if source is not None:
                source.close()
        except (OSError, ValueError, BufferError) as exc:
            error = str(exc)
        finally:
            for s, handler in old.items():
                signal.signal(s, handler)
    elapsed = (args.duration if fixture else max(0, time.monotonic()-start) if start else 0)
    interrupted = STOP is not None or bool(replay_source and replay_source['interrupted'])
    summary = measurement.summary(elapsed, interrupted)
    if fixture:
        summary.update(fixture=True, replay_source=replay_source,
                       quality='failed' if interrupted or (replay_source and replay_source['quality'] == 'failed') else 'unknown',
                       quality_reason=('interrupted' if interrupted else 'source_quality_failed'
                                       if replay_source and replay_source['quality'] == 'failed' else 'fixture_duration_unverified'),
                       expected_samples=None, valid_rate=None, missing_samples_estimate=None, elapsed=None, duration=None,
                       timestamp_resolution_ok=None, coverage_comparison_tolerance=None, coverage_rate_uncertainty=None,
                       timestamp_quantum=None, observed_step_grain=None)
    if error:
        summary.update(quality='failed', error=error, quality_reason='capture_error')
        print(f'eye_measure: {error}', file=sys.stderr)
    emit(summary)
    return 128+STOP if STOP else 1 if error else 0


def number(text):
    value = float(text)
    if not math.isfinite(value) or value < 0:
        raise argparse.ArgumentTypeError('expected a finite nonnegative number')
    return value


def positive(text):
    value = number(text)
    if value == 0:
        raise argparse.ArgumentTypeError('expected a positive number')
    return value


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    cap = commands.add_parser('capture', help='Capture local shared memory or replay JSONL samples')
    cap.add_argument('--condition', required=True, choices=('closed','relaxed','wide','left-wink','right-wink'))
    cap.add_argument('--session', required=True)
    cap.add_argument('--label', required=True)
    cap.add_argument('--source', default='/dev/shm/eye-server.mmap')
    cap.add_argument('--shm-version', default='auto', choices=('auto','4','5'))
    cap.add_argument('--prepare', type=number, default=2)
    cap.add_argument('--duration', type=positive, default=5)
    ev = commands.add_parser('evaluate', help='Evaluate explicitly supplied candidate numbers without saving')
    for name in ('closed','relaxed','wide'):
        ev.add_argument('--'+name, required=True)
    ev.add_argument('--validation-relaxed', action='append', required=True)
    for name in ('margin-left','margin-right','min-base-width','min-wide-width'):
        ev.add_argument('--'+name, type=number if name.startswith('margin') else positive, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'capture':
            return capture(args)
        print(encode(evaluate(args.closed,args.relaxed,args.wide,args.validation_relaxed,
                             args.margin_left,args.margin_right,args.min_base_width,args.min_wide_width)))
        return 0
    except (OSError, ValueError, KeyError, TypeError) as e:
        print(f'eye_measure: {e}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
