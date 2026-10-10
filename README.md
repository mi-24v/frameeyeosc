# frameeyeosc

Reading Steam Frame eye-tracking data (gaze and eye openness) and sending it via OSC.

Against what was suspected early on, the Steam Frame does also track eye openness (lid position) in addition to gaze.
However, this data is only exposed in an internal shared-memory object (`/dev/shm/eye-server.mmap`), and not via any public APIs.
This small headless program reads data sends it out via OSC as teh standard VRCFT parameters, with a configurable prefix.

On the headset, install Rust/Cargo, then build and run normally:

```sh
cargo build --release
runtime_dir=$(mktemp -d /tmp/frameeyeosc.XXXXXX)
./target/release/frameeyeosc --control-socket "$runtime_dir/control.sock" --target 127.0.0.1:9000
```

## Independent Dashboard panel

The separate [frameeyeosc-panel project](https://github.com/mi-24v/frameeyeosc-panel) is a
Linux C++/CMake OpenVR Dashboard application, with its own version 0.1.0 and
pinned official SDK/JSON build dependencies. Build it independently; this Rust
repository is not part of its build. The panel owns a backend child using:

```sh
frameeyeosc-panel --backend /absolute/path/to/frameeyeosc \
  --control-socket /run/user/1000/frameeyeosc-panel/control.sock \
  --config /absolute/path/to/settings.json
```

Use a dedicated user-owned 0700 socket directory and your actual UID/paths.
The child receives the same paths and `--panel-mode`, preserving saved manual
settings. Use the backend executable directly, or an exec-replacing wrapper
that preserves its PID; a forking wrapper fails ownership validation. Existing
backends on the same control socket are refused. Legacy IPC-less instances and
backends using another socket are not detected; stop them manually before
starting the panel. The panel rejects racing foreign peers, keeps running
when the Dashboard/stream disappears, and stops only its owned backend on
Close/local runtime quit. A crashed backend is not restarted; Close and relaunch
manually. It edits the supported settings and shows current/saved/source and
actual tracking/stream/OSC/output state and manual eyelid calibration.
Manifest generation, installation and manual registration/autostart instructions
are in the panel README. The ARM build and launch were checked on Steam Frame, and the operator reported
seeing the calibration controls. Completing calibration, saving/recalibrating on
the headset, keyboard behavior, product manifest registration/autostart, stream
disconnect/recovery, explicit/crash shutdown and IPC reconnect remain unverified
on the headset. See the
[hardware verification scope](docs/research/2026-10-10-calibration-verification.md).

The backend's **incoming request** limit is 64 KiB per LF-terminated line.
A full **outgoing** snapshot can exceed that size because current/saved values
can repeat long strings, within the backend's 1 MiB output queue limit. The
panel therefore bounds incoming response/event lines at 1 MiB and outgoing
requests at 64 KiB. Local x86_64 CMake/CTest and owned-child socket/save smoke
checks do not establish Steam Frame/AArch64 or HMD behavior.

The [eyelid specification](docs/design/2026-10-03-eyelid-calibration-design.md)
and [IPC v1 specification](docs/design/2026-10-06-panel-ipc-design.md) describe
the implemented contracts. [Measurement results](docs/research/2026-10-10-eyelid-measurement.md)
record the observations and limits behind the configurable defaults.

## Command-line arguments

`--target HOST:PORT` sets the OSC destination (default `127.0.0.1:9000`).
`--prefix PATH` sets the avatar-parameter prefix (default `/FT`).
`--target-mode manual|auto` selects destination handling. `--target-host HOST`
and `--target-port PORT` set the manual host and OSC port separately.
An explicit `--target` or `--target-host` selects manual mode, including in
`--panel-mode`; combining either with explicit `--target-mode auto` is an error.
`--target` cannot be combined with the separate host/port options.
Standalone defaults to manual `127.0.0.1:9000`. `--panel-mode` changes the
unspecified target-mode default to auto; a saved manual mode remains manual.
At startup explicit CLI values take precedence over saved values, which take
precedence over defaults. Floating settings (`eyelid_output_cap`, the three
One Euro coefficients, both `eye_widening_margin_*` values, `min_base_width`,
and `min_wide_width`) retain their f64 JSON/CLI values and use the same
normalized floating representation in current and saved snapshots, including
integer-written values. `target_port` remains an integer. Unknown saved data
and arbitrary request params are not converted to floating numbers.
Filter calculations and OSC values use f32 precision, with coefficients checked
for finite positive/nonnegative f32 conversion. `--one-euro` enables filtering;
`--one-euro=false` explicitly overrides a saved true value.

```sh
./target/release/frameeyeosc --target-mode auto --target-port 9000
./target/release/frameeyeosc --shm-version 4 --eyelid-output-cap 0.7
```

Auto mode inspects the executable named `vrlink`, its owned socket inodes,
and `/proc/<pid>/net/udp{,6}` in that process's network namespace. Only owned
connected UDP peers are candidates. The anonymized adapter fixture peer is
`192.0.2.20:10400`; **10400 is a streaming transport port**, so auto uses
only the detected IP with the separately configured OSC port (default 9000).
Loopback, unspecified and multicast addresses are excluded. Global IPv6 is
supported; link-local IPv6 without an interface scope waits for a usable peer.
Multiple distinct peer IPs are ambiguous and leave OSC without a target.
A disappearing candidate process/socket or incomplete proc observation also waits.
Unrelated descriptor churn does not invalidate a verified peer. Absent UDP
family tables are allowed, including kernels with IPv6 disabled; a running
`vrlink (deleted)` executable remains eligible after a Steam binary update.
The next complete observation can recover after a stream restart. Manual
mode keeps its configured destination regardless of stream observations.
If initial manual DNS/socket setup is temporarily unavailable, the bridge
stays in `no_target` and retries once per second until resolved. Malformed
host/IP syntax is rejected before setup. System DNS resolution can block;
the input wait budget does not guarantee a deadline for the OS resolver.
Detection is an inference from the private streaming process, not proof
that the peer is an OSC receiver.

`--shm-version auto|4|5` defaults to auto. Header version and required file size
select the packed layout: ABI 4 uses record offset `0x152` and size `0x4f21a`;
ABI 5 uses `0x157` and `0x4f21f`. Explicit ABI mismatches, missing or uninitialized
input, and file replacement/resize stop tracking output while the bridge stays
resident. Short, zero-version or uninitialized producer files are unavailable,
rather than ABI mismatches. Reopen and target detection are retried at
one-second intervals. Shared sequence futex waits and mutex acquisition share
a 10 ms per-call budget, avoiding a fixed 1 kHz polling loop. This scheduling budget
preserves tracking/filter history on transient no-update polls; one full second
without a producer update confirms inactivity. Inactivity or any invalid/nonfinite
eye sample stops both eyes and resets filter history. Zero openness remains valid.
Finite raw openness excursions beyond [0, 1] retain the existing clamp behavior.
After mutex owner death, the mutex is made consistent and the potentially partial
record sequence is discarded; only a later producer update can be sent.
Header, size and pathname identity are checked around reads; an external
concurrent mmap truncation between checks remains a platform-level hazard.
ABI 5 and automatic destinations have not been validated on headset hardware.

`--eyelid-output-cap VALUE` requires a finite value in [0, 1], default 1.
It clamps both eyelids after optional filtering and calibrated composition.
Without a valid profile, the raw compatibility path retains its existing
normalization and filtering. With cap 1, uncalibrated raw values are unchanged.
Internal output reasons use this priority: `input_unavailable`, `abi_mismatch`,
`no_target`, `invalid_sample`, `tracking_inactive`/`tracking_unknown`, `sending`.
Switching settings or destinations does not send an extra stop notification
to the previous destination. A failed normal tracking-stop notification is
retried for the same destination/prefix until sent or tracking resumes; a
settings switch discards the old pending notification. Repeated OSC error logs
are limited to once per second per error kind.

One Euro filtering is opt-in and smooths gaze and eyelid output after normalization:

```sh
./target/release/frameeyeosc --one-euro
```

`--one-euro-min-cutoff FLOAT` sets stationary smoothing in Hz (default `0.5`; lower is smoother).
`--one-euro-beta FLOAT` controls responsiveness during fast movement (default `3.0`; higher is faster).
`--one-euro-d-cutoff FLOAT` sets derivative smoothing in Hz (default `1.0`).

The defaults follow Baballonia's current One Euro settings. Tuning options are accepted without
`--one-euro`, but have no effect until filtering is enabled.

## Manual eyelid calibration

Start a session, explicitly begin `closed`, then `relaxed`, then either begin
`wide` or skip it. Every stage prepares for 2 seconds and measures for 5 seconds.
The two eye medians are independent; relaxed measurement also records each
maximum from the same valid samples. One active session is allowed. A failed
stage returns to its own waiting state for retry and preserves earlier stages.
Cancellation, including loss of the initiating connection, keeps the old profile.

```json
{"v":1,"type":"request","id":"start","method":"calibration.start","params":{}}
{"v":1,"type":"request","id":"closed","method":"calibration.begin_stage","params":{"session_id":"ID_FROM_START","stage":"closed"}}
{"v":1,"type":"request","id":"relaxed","method":"calibration.begin_stage","params":{"session_id":"ID_FROM_START","stage":"relaxed"}}
{"v":1,"type":"request","id":"skip","method":"calibration.skip_wide","params":{"session_id":"ID_FROM_START"}}
```

Wait for the corresponding stage result before beginning the next stage. Call
`state.get` first to subscribe to events. Each operation response returns actual
`calibration` and `save`; timer events and latest results retain their initiating
`caused_by` IDs. A start/begin response acknowledges acceptance. Completion is
successful only after atomic profile persistence and application.

Measurement needs at least 80% unique valid samples against the observed producer
cadence over the full 5-second window. The median timestamp interval needs at
least ten positive differences; unknown cadence or insufficient timestamp
resolution at the 80% boundary fails the stage for retry. Duplicate sequences
and timestamps cannot increase coverage. Producer restart, backward timestamps,
or input replacement during measurement also fail that stage. Inactive or invalid
samples are missing coverage. Only finite raw openness in [0, 1] is measured;
0 and 1 remain valid. No additional outlier rejection is applied. Cadence is
inferred from observed timestamps, so consistently skipped publishing can bias
that estimate; this does not guarantee a distribution or headset comfort.

Calibrated closed maps to 0 and relaxed to internal Openness=1. Widening begins
at `relaxed_max + margin`; a usable measured wide interval maps to EyeWide=1.
The independent components are optionally filtered before combining
`0.75 * Openness + 0.25 * EyeWide` and applying the final cap. A missing or
insufficient wide interval contributes zero while preserving its base interval
and any measured wide value. Raw wide=1 alone is allowed. With widening disabled,
the base contribution still maps relaxed to 0.75. Wide filters keep updating
while disabled. Recalibration or interval-setting changes reset eyelid history.

```sh
./target/release/frameeyeosc --eye-widening=false --eyelid-output-cap 0.7
./target/release/frameeyeosc --eye-widening-margin-left 0.10 \
  --eye-widening-margin-right 0.10 --min-base-width 0.10 --min-wide-width 0.05
```

`eye_widening` defaults to true. Per-eye margins default to 0.10 and must be
finite and nonnegative. Minimum base and wide widths default to 0.10 and 0.05,
respectively, and must be finite in (0, 1]. All five values are CLI, saved-settings,
and runtime patch keys. Margin or minimum-wide changes can disable a wide interval
without losing its measurement; lowering them can reenable it. A patch that
invalidates an active profile or an already-successful relaxed candidate is
rejected as a whole. An active session rejects every patch containing `shm_version`.

The profile is stored separately at the config path with `.profile.json` appended,
for example `settings.json.profile.json`; the default is
`$XDG_CONFIG_HOME/frameeyeosc/settings.json.profile.json`, or
`$HOME/.config/frameeyeosc/settings.json.profile.json`. It contains only the four
measurements per eye: `closed`, `relaxed`, `relaxed_max`, and optional `wide`.
Completion saves that profile without incidentally saving CLI/runtime settings.
Missing profiles use raw output. Invalid structure or base measurements emit
a diagnostic and use raw output, while allowing a successful recalibration to
overwrite them. A bad `wide` value alone disables only that eye's widening,
reports `invalid_wide`, and keeps both valid base intervals and the other eye's
valid wide measurement.
A failed save retains the previous profile and reports `save_failed`.

## Saved settings and IPC v1

This build implements `state.get`, `settings.patch`, `state.changed`,
`calibration.start`, `calibration.begin_stage`, `calibration.skip_wide`,
`calibration.cancel`, and `calibration.changed` over Unix pathname sockets
using one UTF-8 JSON object per LF-terminated line. Snapshots expose the actual
active profile, derived per-eye intervals, current session, and latest result.

`--config PATH` selects the settings file. Its default is
`$XDG_CONFIG_HOME/frameeyeosc/settings.json`, falling back to
`$HOME/.config/frameeyeosc/settings.json`. Missing files are allowed; malformed
JSON or invalid known saved values are startup errors, even if CLI values would
override them. Settings files are limited to 64 KiB; reading is bounded and
oversized files cause a startup error. Saving a merge that would exceed the
same limit fails before replacement/application. Unknown saved keys are retained
on disk, while snapshots expose only known saved settings. CLI options distinguish omitted
values from explicit false. A runtime patch can override CLI values; the next
startup applies explicit CLI precedence again.

`--control-socket PATH` selects the control socket. The default is
`$XDG_RUNTIME_DIR/frameeyeosc/control.sock`. If XDG_RUNTIME_DIR is absent, an
explicit path is required, including for standalone operation. The immediate
parent must be owned by this user with mode 0700; it is created privately when
absent and its ancestor already exists. Existing directories are validated and
never chmodded. Symlink paths and foreign owners are rejected. Socket and lock
files use mode 0600. A nonblocking per-path lock is acquired before probing and
removing a stale socket; live sockets and regular files are never replaced.
The lock file remains after shutdown to preserve the lock inode.

For the default runtime/config paths:

```sh
./target/release/frameeyeosc --panel-mode --one-euro=false
```

For explicit paths without XDG_RUNTIME_DIR:

```sh
runtime_dir=$(mktemp -d /tmp/frameeyeosc.XXXXXX)
./target/release/frameeyeosc --control-socket "$runtime_dir/control.sock" \
  --config "$HOME/.config/frameeyeosc/settings.json" --target-mode manual
```

Connect and first send `state.get` with a nonempty string ID. Its response
contains `instance_id`, the receiver's `connection_id`, current/saved settings,
per-field sources (`cli`, `saved`, `default`, `panel`), tracking, observed stream
peer, actual OSC target, and output state/reason. Stream `connected` means a
unique peer was detected. Waiting or ambiguous observations map to `unknown`:
absence of a peer cannot establish an intentional disconnect, so this detector
does not claim `disconnected`. Sending means UDP transmission is attempted,
not confirmed reception. Output reasons follow the priority documented above.

```json
{"v":1,"type":"request","id":"s1","method":"state.get","params":{}}
{"v":1,"type":"request","id":"p1","method":"settings.patch","params":{"patch":{"eyelid_output_cap":0.7,"one_euro":false},"save":true}}
```

Supported patch keys are `eye_widening`, `eye_widening_margin_left`,
`eye_widening_margin_right`, `min_base_width`, `min_wide_width`,
`eyelid_output_cap`, `prefix`, `one_euro`,
`one_euro_min_cutoff`, `one_euro_beta`, `one_euro_d_cutoff`, `target_mode`,
`target_host`, `target_port`, and `shm_version`. Values are strictly typed;
`eye_widening` is boolean, margins are finite and nonnegative, and minimum
widths are finite in (0, 1]. `shm_version` is a string `auto`, `4`, or `5`,
and port is an integer 1..65535.
Both `patch` and boolean `save` are required; unknown params/patch keys fail.
The merged candidate is validated and input/UDP resources are prepared before
saving. Only changed patch keys are saved, retaining other saved keys.
A private 0600 temporary file in the same directory is written and fsynced,
then renamed before current settings are applied. Rename is the commit boundary;
file-data syncing does not guarantee directory-entry survival after power loss.
Validation, preparation, or save failure retains old settings and saved values.
ABI failures revalidate the old mapping before reporting continued output.

Patch responses include actual `current`, `sources`, `osc_target`, `output`,
and `save` (`saved`, `not_requested`, `failed`), including on failure. Every
response has object `result`, boolean `ok`, and null or object `error`.
Events begin after the first state response and contain a full snapshot with
the receiver's connection ID. `caused_by` records the original connection and
request ID for a patch; spontaneous tracking/stream changes have null cause.
All requests and runtime ticks run in one sequential loop. A repeated accepted
ID with identical parsed method/params returns its original response without
reapplying or notifying; object key order is irrelevant. Changed content yields
`id_conflict`. Invalid envelopes and unsupported versions are rejected before
cache lookup and are not accepted/cache-registered.

Each connection accepts at most 4096 IDs, retaining all accepted responses
without eviction; new IDs then receive `request_limit`, while existing IDs can
still be retried. Disconnects discard the cache, so reconnect with `state.get`
and reconcile actual state instead of replaying unconfirmed operations.
A client that half-closes its write side still receives responses to every
complete buffered line. The incomplete tail is discarded, future event
subscription stops, and the connection closes after its finite output is flushed.
Malformed JSON, nonfinite numbers, recursive duplicate keys, invalid UTF-8, or
lines over 64 KiB disconnect the peer without partially applying that line.

Implementation resource limits are eight simultaneous clients, 1 MiB queued
output per client, and 16 MiB cached byte payloads per client. Canonical
requests, response lines, and IDs are stored as boxed bytes/strings; this budget
counts their retained lengths. Hash table/entry/allocator overhead is additionally
bounded by 4096 entries, so this budget is not an exact process RSS limit.
Cache space is checked conservatively before mutation with room reserved for
the response; exhaustion disconnects without executing the new request.
Slow clients that exceed the output limit are disconnected. Each poll accepts
at most four connections, reads/writes at most 16 KiB per client, and processes
one line per client (eight requests total), rotating the first client for
fairness. Transient accept or per-client setup failures are logged at most once
per second and keep established clients and OSC running; invalid listener
errors are fatal. These bounds keep IPC work finite between OSC ticks. OS filesystem,
proc scanning, and DNS operations still have no absolute execution deadline.

The shared wire examples in `tests/fixtures/ipc-v1.jsonl` are checked
against a real socket with simulated producer observations and stage time.
IDs are normalized only for comparison; measurement floating values permit
f32 representation differences. Object key order and whitespace are not part of the wire
contract; clients parse JSON rather than comparing raw response bytes. IPC tests
are included as a binary test module at `tests/runtime_ipc/mod.rs` so internal
Runtime paths can use synthetic input without adding a production CLI option
or disabling Cargo's conventional integration-test discovery. Headset/HMD
behavior and real ABI 5 producers remain unverified.

## OSC output

Tracking status is sent as a boolean to:

`/avatar/parameters{prefix}/frame/EyeTrackingActive`

With the default prefix, this is
`/avatar/parameters/FT/frame/EyeTrackingActive`.

The `/frame` namespace avoids conflicts with avatar addons that use
`EyeTrackingActive` internally. Consumers using the previous
`/avatar/parameters{prefix}/EyeTrackingActive` address must update their
OSC mapping.

Gaze and eyelid values remain under
`/avatar/parameters{prefix}/v2/`.

## Raw eye measurement and candidate evaluation

Use the standard-library-only Python tool locally on the headset. Capture each
condition separately; the default preparation is 2 seconds and measurement is
5 seconds. JSONL on stdout contains schema-1 metadata, samples, and a summary,
with the supplied session, condition, and label on every record.

```sh
python3 scripts/eye_measure.py capture --condition closed --session fit-1 --label closed-1 > closed.jsonl
python3 scripts/eye_measure.py capture --condition relaxed --session fit-1 --label relaxed-1 > relaxed.jsonl
python3 scripts/eye_measure.py capture --condition wide --session fit-1 --label wide-1 > wide.jsonl
python3 scripts/eye_measure.py capture --condition relaxed --session fit-1 --label natural-repeat-1 --duration 60 > natural-1.jsonl
```

Keep the calibration captures fixed while collecting independent natural-open
eye validation, including normal blinks. Use a new label for each repeat, and a
new session after removing and putting on the headset again. Optional
`left-wink` and `right-wink` conditions help inspect asymmetry. `--shm-version`
accepts `auto` (default), `4`, or `5`; incompatible headers or sizes are rejected.
`--source FILE.jsonl` replays existing raw samples without accessing hardware;
replay cannot certify measurement duration and reports quality as unknown,
or failed for an interrupted/failed source. `replay_source` retains the original
condition, session, label, quality, and interruption information. Null values
from sanitized nonfinite samples are counted as invalid and replay continues.
Replay leaves duration, elapsed time, expected samples, valid coverage, and
missing-sample estimate null because the original measurement duration is unverified.
Interrupted replay retains the interrupted reason; failed source quality uses
`source_quality_failed`. JSONL requires an object on every nonblank line;
input errors identify the file and line and produce a failed capture summary.

Compare explicit candidates offline; all four numbers are required. The values
below are example candidates, not product defaults or an automatic selection.
Repeated validation arguments allow natural repeats and refitted sessions to
be evaluated against the same calibration.

```sh
python3 scripts/eye_measure.py evaluate \
  --closed closed.jsonl --relaxed relaxed.jsonl --wide wide.jsonl \
  --validation-relaxed natural-1.jsonl --validation-relaxed natural-refit.jsonl \
  --margin-left 0.05 --margin-right 0.02 \
  --min-base-width 0.05 --min-wide-width 0.05
```

The summary reports each eye's calibration medians, relaxed maximum,
`wide_start = relaxed_max + margin`, usable intervals, and validation
`EyeWide > 0` fraction. Distribution summaries cover EyeWide and
`min(1, 0.75 * base + 0.25 * EyeWide)`. Summary-only historical fixtures can
provide calibration representatives, but have no validation raw samples:
their leak rate is null and status is unknown. Source objects retain path,
condition, session, label, quality, interruption, and summary availability.
Known condition mismatches are rejected. For legacy inputs, a label that
exactly matches a condition name supplies the condition for this check;
arbitrary labels are not inferred and have `condition_verified: false`.
Legacy inputs without a quality field
remain usable, with `quality: unknown` and `legacy_quality_missing`; unfinished
inputs have `summary_missing`. `source_quality_ok` is false for these inputs
and failed or interrupted captures. Numerical raw observations from such
inputs have validation status `unqualified`, rather than a quality approval.
Per-file validation reports allow each label and refitted session to be
compared separately. Sources record device/inode file identity. Validation
files matching a calibration inode (including symbolic and hard links), or
sharing its nonnull session/label and the same condition, have
`validation_overlaps_calibration: true`. Their raw observations remain visible
but are unqualified as independent validation. Mixed raw/summary-only validation
sets have `aggregate_partial: true`: the displayed distributions cover the raw
subset, overall availability is false, and the overall leak rate is unknown.
Per-file observations remain separately available. Duplicate validation files
or matching measurement identities are included only once in the aggregate;
`duplicate_validation_input`, `included_in_aggregate`, and `duplicate_of` identify
the excluded copies. Their presence makes the aggregate unqualified while
retaining observed leak rates and per-file results.
Unknown reasons distinguish missing raw data and unusable
base/wide intervals. When only wide is unusable, EyeWide is disabled (0), the
base distribution still contributes 0.75, and the leak rate remains unknown.
No calibration is saved.

Capture accepts a snapshot only when tracking is active and both eyes contain
finite values in [0, 1]. Unique finite producer timestamps establish observed
cadence from at least ten positive differences in a monotonic epoch. Estimated
expected samples use the full requested duration, so an observed 90 Hz stream
that stops early loses coverage. Quality requires at least 5 completed seconds
and 80% valid coverage; short debug captures and interrupted runs cannot pass.
Coverage uncertainty combines floating-point timestamp spacing (`math.ulp`) and
observed separations between steps shorter than 1.5 times the median period.
Longer skipped-update multiples and stop/resume gaps are excluded from grain
estimation, while coverage still uses the full measurement duration. Step
variants within twice the endpoint float spacing are treated as representation
noise. This estimates observed grain; uniform timestamps or unobserved step
sizes cannot establish the underlying hardware clock grain. Quality still
assumes the observed cadence.
Coverage outside the resulting uncertainty band is passed or failed directly.
Within the band around 80%, roundoff correction is permitted only below half
one sample's coverage; wider ambiguity produces `unknown` with
`timestamp_resolution_insufficient`. Long captures far from the boundary remain
decidable. The summary exposes observed grain and coverage uncertainty.
`timestamp_resolution_ok` describes single-sample boundary precision, rather
than whether a clearly high/low coverage can be classified; it is null when no
period is known. Replay clock diagnostics are all null. 360/450 passes and
359/450 fails at tested clock origins 0, 4e6, and 1.8e9; interruption still fails
at any resolution.
Missing cadence or a producer restart yields unknown quality. This is an
observed cadence estimate: repeated skipped updates, genuinely mixed producer
periods, and a consistently slow producer can bias its median. A passed result
is not proof of the hardware's nominal rate.
Inspect counters, raw samples, and repetitions when assessing reliability.
The missing-sample estimate uses `expected_samples - valid_snapshots`, including
inactive/invalid updates in missing valid coverage, and explicitly names this
basis. Preparation establishes the baseline and is excluded from the measured
sample distribution. Capture errors emit a failed summary with an error field;
a stop during mutex waiting is classified as cancellation rather than busy.
Ctrl-C emits an interrupted summary and exits with status 130; the bounded
mutex wait is at most 0.5 seconds and resources are released on exit.
After mutex owner death, capture makes the mutex consistent, requests a new
update, and discards the potentially partial record and its sequence, matching
the Rust reader; measured discards count as `owner_dead_discarded`, never as
updates or valid samples.

Local ABI and lifecycle tests do not replace headset validation. The capture
reader checks header and file size before and after snapshots; concurrent
external truncation of an mmap between checks remains a platform-level hazard.

The shared control header for ABI 4/5 is supported by the fixed reference
[sasaken1102r/frameeyeosc at af2610e](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs#L125-L166):
both layouts place the control header at zero, with mutex at 0x08, sequence at
0x38, and request at 0x3c, as checked by its
[offset assertions](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs#L214-L223).
This is source-level ABI evidence; local synthetic tests and that reference do
not establish successful access to an actual ABI 5 producer.
