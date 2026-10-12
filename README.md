# frameeyeosc

[日本語 README](README_ja.md)

frameeyeosc reads Steam Frame gaze and eye openness (lid position) and sends
them over OSC using standard VRCFT parameters with a configurable prefix.
It runs without a GUI. Eye openness is exposed through the internal
shared-memory object (`/dev/shm/eye-server.mmap`), not through a public API.

The tracking data writer is the process that writes eye-tracking values and
update timestamps into this shared memory. This name describes its role;
the exact writing process has not been identified. Raw eye-tracking parameters
are the values read from shared memory before frameeyeosc applies calibration
or filtering. They are not necessarily raw sensor signals; processing inside
the tracking data writer has not been verified.

## Build and run

Install Rust/Cargo on the headset, then build and run:

```sh
cargo build --release
runtime_dir=$(mktemp -d /tmp/frameeyeosc.XXXXXX)
./target/release/frameeyeosc --control-socket "$runtime_dir/control.sock" --target 127.0.0.1:9000
```

## OSC output

### Parameter addresses

Each address starts with `/avatar/parameters{prefix}`. The default prefix is
`/FT`; trailing slashes in the configured prefix are removed.

| Address suffix | OSC type | Value |
|---|---|---|
| `/frame/EyeTrackingActive` | boolean | `true` before each set of tracking values; `false` for a tracking-stop notification. |
| `/v2/EyeLeftX` | float32 | Left-eye horizontal gaze, in [-1, 1]. |
| `/v2/EyeLeftY` | float32 | Left-eye vertical gaze, in [-1, 1]. |
| `/v2/EyeRightX` | float32 | Right-eye horizontal gaze, in [-1, 1]. |
| `/v2/EyeRightY` | float32 | Right-eye vertical gaze, in [-1, 1]. |
| `/v2/EyeLidLeft` | float32 | Left eyelid output, in [0, `eyelid_output_cap`]. |
| `/v2/EyeLidRight` | float32 | Right eyelid output, in [0, `eyelid_output_cap`]. |
| `/v2/EyeX` | float32 | Combined horizontal gaze derived from the fixation point, in [-1, 1]. |
| `/v2/EyeY` | float32 | Combined vertical gaze derived from the fixation point, in [-1, 1]. |

For example, the default left-eyelid address is
`/avatar/parameters/FT/v2/EyeLidLeft`, and the tracking-status address is
`/avatar/parameters/FT/frame/EyeTrackingActive`.

Gaze angles use `atan2(x, -z)` and `atan2(y, -z)`, scaled so that 45 degrees
maps to 1 and -45 degrees to -1, then clamped to [-1, 1]. Positive Y represents
up in the VRCFT convention; this orientation remains unverified on hardware.
Optional One Euro filtering smooths gaze and eyelid output. Eyelids use either
raw normalized openness or the calibrated composition, then the final cap;
see [eyelid output and filtering](#eyelid-output-and-filtering) and
[manual eyelid calibration](#manual-eyelid-calibration).

### Tracking status and delivery

A normal update sends `EyeTrackingActive` as true, followed by the eight float
parameters above. Each parameter is a separate OSC message with one argument;
the messages are not sent as an atomic bundle. A send error can interrupt the
set after some messages have been sent.

A tracking-stop notification sends only `EyeTrackingActive` as false; it does
not send zero gaze or eyelid values. Failed stop notifications are retried for
the same destination and prefix. Without an OSC destination, no notification
can be sent. Changing settings or destinations does not send an extra stop
notification to the previous destination and discards the old pending stop.
See [output state and stop notifications](#output-state-and-stop-notifications).
UDP transmission does not confirm reception by an avatar or OSC receiver.

### Tracking address migration

The `/frame` namespace avoids conflicts with avatar addons that use
`EyeTrackingActive` internally. Consumers using the previous
`/avatar/parameters{prefix}/EyeTrackingActive` address must update their
OSC mapping.

## Command-line arguments

Defaults below apply when no saved value or explicit CLI value is present.

### OSC destination

| Argument | Default | Description |
|---|---|---|
| `--target HOST:PORT` | `127.0.0.1:9000` | Set the manual OSC destination. |
| `--target-mode manual\|auto` | `manual`; `auto` in panel mode | Select a manual destination or infer the streaming peer IP. |
| `--target-host HOST` | `127.0.0.1` | Set the manual host and select manual mode. |
| `--target-port PORT` | `9000` | Set the OSC port (1–65535), independently of the streaming port. |
| `--prefix PATH` | `/FT` | Set the avatar-parameter prefix. |

### Shared-memory input

| Argument | Default | Description |
|---|---|---|
| `--shm-version auto\|4\|5` | `auto` | Select the ABI or infer it from the header. |

### Eyelid calibration and output

| Argument | Default | Description |
|---|---|---|
| `--eye-widening[=BOOL]` | `true` | Include the calibrated widening contribution. |
| `--eye-widening-margin-left FLOAT` | `0.10` | Set the finite, nonnegative raw margin above the left relaxed maximum. |
| `--eye-widening-margin-right FLOAT` | `0.10` | Set the finite, nonnegative raw margin above the right relaxed maximum. |
| `--min-base-width FLOAT` | `0.10` | Set the minimum closed-to-relaxed width; finite and in (0, 1]. |
| `--min-wide-width FLOAT` | `0.05` | Set the minimum widening interval width; finite and in (0, 1]. |
| `--eyelid-output-cap VALUE` | `1` | Clamp final eyelid output; finite and in [0, 1]. |

See [manual eyelid calibration](#manual-eyelid-calibration) for profile creation
and how these settings affect calibrated output.

### One Euro filtering

| Argument | Default | Description |
|---|---|---|
| `--one-euro[=BOOL]` | `false` | Enable smoothing for gaze and eyelid output. |
| `--one-euro-min-cutoff FLOAT` | `0.5` | Set stationary smoothing in Hz; finite and positive. Lower is smoother. |
| `--one-euro-beta FLOAT` | `3.0` | Set movement responsiveness; finite and nonnegative. Higher is faster. |
| `--one-euro-d-cutoff FLOAT` | `1.0` | Set derivative smoothing in Hz; finite and positive. |

The filter defaults follow Baballonia's One Euro settings. Tuning options are
accepted while filtering is disabled, but take effect only when it is enabled.

### Settings and panel integration

| Argument | Default | Description |
|---|---|---|
| `--config PATH` | `$XDG_CONFIG_HOME/frameeyeosc/settings.json` | Select saved settings; fall back to `$HOME/.config/frameeyeosc/settings.json`. |
| `--control-socket PATH` | `$XDG_RUNTIME_DIR/frameeyeosc/control.sock` | Select the IPC socket in a user-owned 0700 directory. An explicit path is required without `XDG_RUNTIME_DIR`. |
| `--panel-mode` | Off | Default to auto target mode when no explicit or saved target mode applies. |
| `-h`, `--help` | — | Print command-line help. |

See [saved settings and IPC v1](#saved-settings-and-ipc-v1) for path validation,
settings persistence, and runtime updates.

### Constraints and precedence

An explicit `--target` or `--target-host` selects manual mode, including in
`--panel-mode`; combining either with explicit `--target-mode auto` is an error.
`--target` cannot be combined with the separate host/port options.
Standalone defaults to manual `127.0.0.1:9000`. `--panel-mode` changes the
unspecified target-mode default to auto; a saved manual mode remains manual.
At startup explicit CLI values take precedence over saved values, which take
precedence over defaults. Boolean options accept `true` or `false` with `=`;
omitting the value enables the option. `--one-euro=false` explicitly overrides
a saved true value; `--eye-widening=false` disables widening.

### Examples

```sh
./target/release/frameeyeosc --target-mode auto --target-port 9000
./target/release/frameeyeosc --shm-version 4 --eyelid-output-cap 0.7
./target/release/frameeyeosc --one-euro
```

## Saved settings and IPC v1

### Protocol

IPC v1 uses Unix pathname sockets with one UTF-8 JSON object per LF-terminated
line.

| Method or event | Type | Purpose |
|---|---|---|
| `state.get` | Request | Read the current snapshot and subscribe to events. |
| `settings.patch` | Request | Change settings at runtime and optionally save them. |
| `state.changed` | Event | Receive updated state snapshots. |
| `calibration.start` | Request | Start an eyelid calibration session. |
| `calibration.begin_stage` | Request | Begin a `closed`, `relaxed`, or `wide` stage. |
| `calibration.skip_wide` | Request | Complete calibration without measuring wide. |
| `calibration.cancel` | Request | Cancel the session and retain the previous profile. |
| `calibration.changed` | Event | Receive calibration progress and results. |

Snapshots expose the actual
active profile, derived per-eye intervals, current session, and latest result.

### Settings file and startup precedence

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

Floating settings (`eyelid_output_cap`, the three One Euro coefficients, both
`eye_widening_margin_*` values, `min_base_width`, and `min_wide_width`) retain
their f64 JSON/CLI values and use the same normalized floating representation
in current and saved snapshots, including integer-written values.
`target_port` remains an integer. Unknown saved data and arbitrary request
params are not converted to floating numbers.

### Socket requirements

`--control-socket PATH` selects the control socket. The default is
`$XDG_RUNTIME_DIR/frameeyeosc/control.sock`. If XDG_RUNTIME_DIR is absent, an
explicit path is required, including for standalone operation.

- The immediate parent must be owned by this user with mode 0700. It is created
  privately when absent and its ancestor already exists. Existing directories
  are validated and never chmodded.
- Symlink paths and foreign owners are rejected. Socket and lock files use
  mode 0600.
- A nonblocking per-path lock is acquired before probing and removing a stale
  socket. Live sockets and regular files are never replaced.
- The lock file remains after shutdown to preserve the lock inode.

### Launch examples

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

### Connect and read state

Connect and first send `state.get` with a nonempty string ID. Its response
contains `instance_id`, the receiver's `connection_id`, current/saved settings,
per-field sources (`cli`, `saved`, `default`, `panel`), tracking, observed stream
peer, actual OSC target, and output state/reason. Stream `connected` means a
unique peer was detected. Waiting or ambiguous observations map to `unknown`:
absence of a peer cannot establish an intentional disconnect, so this detector
does not claim `disconnected`. Sending means UDP transmission is attempted,
not confirmed reception. See [output state and stop notifications](#output-state-and-stop-notifications)
for the output reason priority.

```json
{"v":1,"type":"request","id":"s1","method":"state.get","params":{}}
{"v":1,"type":"request","id":"p1","method":"settings.patch","params":{"patch":{"eyelid_output_cap":0.7,"one_euro":false},"save":true}}
```

### Changing settings at runtime

Use `settings.patch` to change these settings while the bridge is running.
Set `save` to false to apply them only to the current process, or true to also
persist the changed patch keys. Defaults and CLI equivalents are listed under
[command-line arguments](#command-line-arguments).

| Key | JSON type | Constraints |
|---|---|---|
| `eye_widening` | boolean | Enable or disable the calibrated widening contribution. |
| `eye_widening_margin_left` | number | Finite and nonnegative. |
| `eye_widening_margin_right` | number | Finite and nonnegative. |
| `min_base_width` | number | Finite and in (0, 1]. |
| `min_wide_width` | number | Finite and in (0, 1]. |
| `eyelid_output_cap` | number | Finite and in [0, 1]. |
| `prefix` | string | Starts with `/`; contains no whitespace or control characters. |
| `one_euro` | boolean | Enable or disable filtering. |
| `one_euro_min_cutoff` | number | Finite and positive, including after f32 conversion. |
| `one_euro_beta` | number | Finite and nonnegative; finite after f32 conversion. |
| `one_euro_d_cutoff` | number | Finite and positive, including after f32 conversion. |
| `target_mode` | string | `manual` or `auto`. |
| `target_host` | string | Valid IP, scoped IPv6, or hostname syntax; DNS availability is checked separately. |
| `target_port` | integer | 1 through 65535. |
| `shm_version` | string | `auto`, `4`, or `5`; rejected during an active calibration session. |

Calibration interval changes must also preserve valid base measurements in an
active profile or an already-successful relaxed candidate. An invalid patch is
rejected as a whole. Numeric JSON values may be written as integers or decimals;
`target_port` requires an integer.

### Saving runtime changes

Both `patch` and boolean `save` are required; unknown params/patch keys fail.
The merged candidate is validated and input/UDP resources are prepared before
saving. Only changed patch keys are saved, retaining other saved keys.
A private 0600 temporary file in the same directory is written and fsynced,
then renamed before current settings are applied. Rename is the commit boundary;
file-data syncing does not guarantee directory-entry survival after power loss.
Validation, preparation, or save failure retains old settings and saved values.
ABI failures revalidate the old mapping before reporting continued output.

### Responses, events, and request IDs

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

### Reconnection and invalid input

Each connection accepts at most 4096 IDs, retaining all accepted responses
without eviction; new IDs then receive `request_limit`, while existing IDs can
still be retried. Disconnects discard the cache, so reconnect with `state.get`
and reconcile actual state instead of replaying unconfirmed operations.
A client that half-closes its write side still receives responses to every
complete buffered line. The incomplete tail is discarded, future event
subscription stops, and the connection closes after its finite output is flushed.
Malformed JSON, nonfinite numbers, recursive duplicate keys, invalid UTF-8, or
lines over 64 KiB disconnect the peer without partially applying that line.

### Resource limits and scheduling

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

### Local protocol tests

The shared wire examples in `tests/fixtures/ipc-v1.jsonl` are checked
against a real socket with simulated tracking data observations and stage time.
IDs are normalized only for comparison; measurement floating values permit
f32 representation differences. Object key order and whitespace are not part of the wire
contract; clients parse JSON rather than comparing raw response bytes. IPC tests
are included as a binary test module at `tests/runtime_ipc/mod.rs` so internal
Runtime paths can use synthetic input without adding a production CLI option
or disabling Cargo's conventional integration-test discovery. Headset/HMD
behavior and access to real ABI 5 tracking data writers remain unverified.

## Manual eyelid calibration

### Session workflow

Start a session, explicitly begin `closed`, then `relaxed`, then either begin
`wide` or skip it. Every stage prepares for 2 seconds and measures for 5 seconds.
The two eye medians are independent. Each stage records:

| Stage | Preparation | Measurement | Values per eye |
|---|---|---|---|
| `closed` | 2 seconds | 5 seconds | Median raw openness. |
| `relaxed` | 2 seconds | 5 seconds | Median and maximum from the same valid samples. |
| `wide` (optional) | 2 seconds | 5 seconds | Median raw openness. |

One active session is allowed. A failed
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

### Measurement requirements

Measurement needs at least 80% unique valid samples against the observed update
cadence over the full 5-second window. The median timestamp interval needs at
least ten positive differences; unknown cadence or insufficient timestamp
resolution at the 80% boundary fails the stage for retry. Duplicate sequences
and timestamps cannot increase coverage. A tracking data writer restart, backward timestamps,
or input replacement during measurement also fail that stage. Inactive or invalid
samples are missing coverage. Only finite raw openness in [0, 1] is measured;
0 and 1 remain valid. No additional outlier rejection is applied. Cadence is
inferred from observed timestamps, so consistently skipped publishing can bias
that estimate; this does not guarantee a distribution or headset comfort.

### Calibrated output

Calibrated closed maps to 0 and relaxed to internal Openness=1. Widening begins
at `relaxed_max + margin`; a usable measured wide interval maps to EyeWide=1.
The independent components are optionally filtered before combining
`0.75 * Openness + 0.25 * EyeWide` and applying the final cap. A missing or
insufficient wide interval contributes zero while preserving its base interval
and any measured wide value. Raw wide=1 alone is allowed. With widening disabled,
the base contribution still maps relaxed to 0.75. Wide filters keep updating
while disabled. Recalibration or interval-setting changes reset eyelid history.

### Tuning and runtime changes

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

### Profile storage and recovery

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

## Independent Dashboard panel

The separate [frameeyeosc-panel project](https://github.com/mi-24v/frameeyeosc-panel) is a
Linux C++/CMake OpenVR Dashboard application, with its own version 0.1.0 and
pinned official SDK/JSON build dependencies. Build it independently of this
Rust repository. The panel edits supported settings, provides manual eyelid
calibration, and displays current and saved values, their sources, and actual
tracking, stream, OSC, and output state.

### Launch

The panel starts and owns a backend child:

```sh
frameeyeosc-panel --backend /absolute/path/to/frameeyeosc \
  --control-socket /run/user/1000/frameeyeosc-panel/control.sock \
  --config /absolute/path/to/settings.json
```

The child receives the same paths and `--panel-mode`, preserving saved manual
settings.
Manifest generation, installation and manual registration/autostart instructions
are in the panel README.

### Usage requirements

- Use a dedicated user-owned 0700 socket directory and your actual UID/paths.
- Use the backend executable directly, or an exec-replacing wrapper that
  preserves its PID. A forking wrapper fails ownership validation.
- Stop legacy IPC-less instances and backends using another socket manually
  before starting the panel; it cannot detect them. It refuses existing
  backends on the same control socket and rejects racing foreign peers.

### Shutdown and recovery

The panel keeps running when the Dashboard or stream disappears.
Close or local runtime quit stops only the backend owned by the panel.
A crashed backend is not restarted automatically; use Close and relaunch manually.

### Hardware verification

The ARM build and launch were checked on Steam Frame, and the operator reported
seeing the calibration controls. Completing calibration, saving/recalibrating on
the headset, keyboard behavior, product manifest registration/autostart, stream
disconnect/recovery, explicit/crash shutdown and IPC reconnect remain unverified
on the headset. See the
[hardware verification scope](docs/research/2026-10-10-calibration-verification.md).

Local x86_64 CMake/CTest and owned-child socket/save smoke checks do not
establish Steam Frame/AArch64 or HMD behavior.

### IPC size limits

The backend's incoming request limit is 64 KiB per LF-terminated line.
A full outgoing snapshot can exceed that size because current/saved values
can repeat long strings, within the backend's 1 MiB output queue limit. The
panel therefore bounds incoming response/event lines at 1 MiB and outgoing
requests at 64 KiB.

## Destination detection and recovery

### Automatic destination selection

Auto mode inspects the executable named `vrlink`, its owned socket inodes,
and `/proc/<pid>/net/udp{,6}` in that process's network namespace. Only owned
connected UDP peers are candidates. The anonymized adapter fixture peer is
`192.0.2.20:10400`; 10400 is a streaming transport port, so auto uses
only the detected IP with the separately configured OSC port (default 9000).

### Peer eligibility

Loopback, unspecified and multicast addresses are excluded. Global IPv6 is
supported; link-local IPv6 without an interface scope waits for a usable peer.
Multiple distinct peer IPs are ambiguous and leave OSC without a target.
A disappearing candidate process/socket or incomplete proc observation also waits.
Unrelated descriptor churn does not invalidate a verified peer. Absent UDP
family tables are allowed, including kernels with IPv6 disabled; a running
`vrlink (deleted)` executable remains eligible after a Steam binary update.
The next complete observation can recover after a stream restart. Manual
mode keeps its configured destination regardless of stream observations.

### Manual destination retries and detection limits

If initial manual DNS/socket setup is temporarily unavailable, the bridge
stays in `no_target` and retries once per second until resolved. Malformed
host/IP syntax is rejected before setup. System DNS resolution can block;
the input wait budget does not guarantee a deadline for the OS resolver.
Detection is an inference from the private streaming process, not proof
that the peer is an OSC receiver.

## Shared-memory input and recovery

### ABI selection

`--shm-version` defaults to auto. Header version and required file size
select the packed layout: ABI 4 uses record offset `0x152` and size `0x4f21a`;
ABI 5 uses `0x157` and `0x4f21f`.

### Unavailable input and retries

Explicit ABI mismatches, missing or uninitialized
input, and file replacement/resize stop tracking output while the bridge stays
resident. Files with headers shorter than 8 bytes, a zero version, or an
uninitialized control header are unavailable rather than ABI mismatches.
A valid version header with less than the required ABI size is an ABI mismatch.
Reopen and target detection are retried at
one-second intervals.

### Update timing and sample validity

Shared sequence futex waits and mutex acquisition share
a 10 ms per-call budget, avoiding a fixed 1 kHz polling loop. This scheduling budget
preserves tracking/filter history on transient no-update polls; one full second
without a tracking data update confirms inactivity. Inactivity or any invalid/nonfinite
eye sample stops both eyes and resets filter history. Zero openness remains valid.
Finite raw openness excursions beyond [0, 1] retain the existing clamp behavior.

### Tracking data interruptions and recovery

After mutex owner death, the mutex is made consistent and the potentially partial
record sequence is discarded; only a later tracking data update can be sent.
Header, size and pathname identity are checked around reads; an external
concurrent mmap truncation between checks remains a platform-level hazard.
ABI 5 and automatic destinations have not been validated on headset hardware.

## Eyelid output and filtering

### Output cap and smoothing

`--eyelid-output-cap` clamps both eyelids after optional filtering and calibrated composition.
Without a valid profile, the raw compatibility path retains its existing
normalization and filtering. With cap 1, uncalibrated raw values are unchanged.

One Euro filtering is opt-in and smooths gaze and eyelid output after normalization.
Filter calculations and OSC values use f32 precision, with coefficients checked
for finite positive/nonnegative f32 conversion.

### Output state and stop notifications

The first matching condition determines the output reason:

| Priority | Reason | Condition |
|---|---|---|
| 1 | `input_unavailable` | Shared-memory input is unavailable. |
| 2 | `abi_mismatch` | Input has an incompatible ABI. |
| 3 | `no_target` | No OSC destination is available. |
| 4 | `invalid_sample` | Input contains an invalid eye sample. |
| 5 | `tracking_inactive` / `tracking_unknown` | Tracking is false / no tracking state is known. |
| 6 | `sending` | Tracking is active and an OSC destination is available. |

These reasons describe runtime state, not confirmed UDP delivery. Without a
destination, the bridge cannot send an OSC tracking-stop notification.
Switching settings or destinations does not send an extra stop notification
to the previous destination. A failed normal tracking-stop notification is
retried for the same destination/prefix until sent or tracking resumes; a
settings switch discards the old pending notification. Repeated OSC error logs
are limited to once per second per error kind.

## Raw eye-tracking parameter measurement and candidate evaluation

### Capture conditions

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
`left-wink` and `right-wink` conditions help inspect asymmetry.

### Input selection and replay

`--shm-version` accepts `auto` (default), `4`, or `5`;
incompatible headers or sizes are rejected.
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

### Evaluate explicit candidates

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

### Evaluation results and source quality

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

### Independent validation and aggregate results

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

### Capture quality and coverage

Capture accepts a snapshot only when tracking is active and both eyes contain
finite values in [0, 1]. Unique finite timestamps from the tracking data writer establish observed
cadence from at least ten positive differences in a monotonic epoch. Estimated
expected samples use the full requested duration, so an observed 90 Hz stream
that stops early loses coverage. Quality requires at least 5 completed seconds
and 80% valid coverage; short debug captures and interrupted runs cannot pass.

### Timestamp resolution

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

### Cadence limits and missing samples

Missing cadence or a tracking data writer restart yields unknown quality. This is an
observed cadence estimate: repeated skipped updates, genuinely mixed update
periods, and a consistently slow tracking data writer can bias its median. A passed result
is not proof of the hardware's nominal rate.
Inspect counters, raw samples, and repetitions when assessing reliability.
The missing-sample estimate uses `expected_samples - valid_snapshots`, including
inactive/invalid updates in missing valid coverage, and explicitly names this
basis. Preparation establishes the baseline and is excluded from the measured
sample distribution.

### Cancellation and tracking data interruptions

Capture errors emit a failed summary with an error field;
a stop during mutex waiting is classified as cancellation rather than busy.
Ctrl-C emits an interrupted summary and exits with status 130; the bounded
mutex wait is at most 0.5 seconds and resources are released on exit.
After mutex owner death, capture makes the mutex consistent, requests a new
update, and discards the potentially partial record and its sequence, matching
the Rust reader; measured discards count as `owner_dead_discarded`, never as
updates or valid samples.

### ABI evidence and hardware limits

Local ABI and lifecycle tests do not replace headset validation. The capture
reader checks header and file size before and after snapshots; concurrent
external truncation of an mmap between checks remains a platform-level hazard.

The shared control header for ABI 4/5 is supported by the fixed reference
[sasaken1102r/frameeyeosc at af2610e](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs#L125-L166):
both layouts place the control header at zero, with mutex at 0x08, sequence at
0x38, and request at 0x3c, as checked by its
[offset assertions](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/src/main.rs#L214-L223).
This is source-level ABI evidence; local synthetic tests and that reference do
not establish successful access to an actual ABI 5 tracking data writer.

## Specifications and measurements

The [eyelid specification](docs/design/2026-10-03-eyelid-calibration-design.md)
and [IPC v1 specification](docs/design/2026-10-06-panel-ipc-design.md) describe
the implemented contracts. [Measurement results](docs/research/2026-10-10-eyelid-measurement.md)
record the observations and limits behind the configurable defaults.

## Acknowledgments

Thanks to konsti219 for the original frameeyeosc project, and to sasaken1102r
for the fork used as a reference for ABI layouts and eyelid calibration behavior:
[sasaken1102r/frameeyeosc at af2610e](https://github.com/sasaken1102r/frameeyeosc/tree/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d).
That reference is published under the
[MIT License](https://github.com/sasaken1102r/frameeyeosc/blob/af2610e4c396ca9d6fc47fe0054b9ae7c1d6d85d/LICENSE),
with copyright notices for konsti219 (2026) and sasaken1102r (2026, changes in
the fork). The original project's copyright and MIT license text are retained
in this repository's [LICENSE](LICENSE).
