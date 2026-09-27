//! Steam Frame 0.5.0 eye bridge using the private version-4 shared-memory ABI.

mod filter;

use clap::Parser;
use filter::{EyeValues, OneEuroConfig, OptionalEyeFilter};
use memmap2::{MmapMut, MmapOptions};
use rosc::{OscMessage, OscPacket, OscType, encoder};
use std::error::Error;
use std::fs::OpenOptions;
use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::ptr;
use std::time::Duration;

const SHM_VERSION: u32 = 4;
const SHM_SIZE: usize = 0x4f21a;
const SOURCE: &str = "/dev/shm/eye-server.mmap";
const TIMEOUT: Duration = Duration::from_secs(1);

#[repr(C)]
struct EyeServerMmap {
    version: u32,
    initialized: u32,
    // The target glibc mutex slot is 48 bytes; host libc may define a smaller type.
    metadata_mutex: [u8; 0x30],
    sequence: u32,
    metadata_requested: u32,
    other_control_fields: [u8; 0x112],
    eye_data: EyeDataMmap,
}

// The record is packed, so its timestamp and vectors are not naturally aligned.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct EyeDataMmap {
    producer_state: u32,
    sample_flag: u8,
    sample_time: f64,
    // Left, right; after stereo fusion.
    gaze_direction: [[f32; 3]; 2],
    gaze_covariance_diag: [[f32; 3]; 2],
    // Head-relative, -Z forward, in metres.
    fixation_point: [f32; 3],
    pre_fusion_gaze: [[f32; 3]; 2],
    pre_fusion_cov_diag: [[f32; 3]; 2],
    openness: [f32; 2],
    estimate_extra: [f32; 8],
    reserved: [u8; 0xe1b],
}

const _: () = {
    assert!(offset_of!(EyeServerMmap, metadata_mutex) == 0x08);
    assert!(offset_of!(EyeServerMmap, sequence) == 0x38);
    assert!(offset_of!(EyeServerMmap, metadata_requested) == 0x3c);
    assert!(offset_of!(EyeServerMmap, eye_data) == 0x152);
    assert!(offset_of!(EyeDataMmap, sample_time) == 0x05);
    assert!(offset_of!(EyeDataMmap, gaze_direction) == 0x0d);
    assert!(offset_of!(EyeDataMmap, gaze_covariance_diag) == 0x25);
    assert!(offset_of!(EyeDataMmap, fixation_point) == 0x3d);
    assert!(offset_of!(EyeDataMmap, pre_fusion_gaze) == 0x49);
    assert!(offset_of!(EyeDataMmap, pre_fusion_cov_diag) == 0x61);
    assert!(offset_of!(EyeDataMmap, openness) == 0x79);
    assert!(size_of::<EyeDataMmap>() == 0xebc);
    assert!(size_of::<EyeServerMmap>() <= SHM_SIZE);
    assert!(size_of::<libc::pthread_mutex_t>() <= 0x30);
    assert!(8 % align_of::<libc::pthread_mutex_t>() == 0);
};

#[derive(Parser)]
#[command(about = "Send Steam Frame eye tracking from shared memory over OSC")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9000")]
    target: String,
    #[arg(long, default_value = "/FT")]
    prefix: String,
    /// Enable One Euro smoothing for gaze and eyelid output.
    #[arg(long)]
    one_euro: bool,
    /// Minimum cutoff in Hz (lower is smoother at rest; Baballonia default: 0.5).
    #[arg(long, default_value_t = 0.5, value_parser = parse_positive_f32)]
    one_euro_min_cutoff: f32,
    /// Speed coefficient (higher is more responsive; Baballonia default: 3.0).
    #[arg(long, default_value_t = 3.0, value_parser = parse_nonnegative_f32)]
    one_euro_beta: f32,
    /// Derivative low-pass cutoff in Hz (canonical/Baballonia default: 1.0).
    #[arg(long, default_value_t = 1.0, value_parser = parse_positive_f32)]
    one_euro_d_cutoff: f32,
}

fn parse_positive_f32(value: &str) -> Result<f32, String> {
    let value = value
        .parse::<f32>()
        .map_err(|_| format!("expected a finite number greater than zero, got {value:?}"))?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(format!(
            "expected a finite number greater than zero, got {value}"
        ))
    }
}

fn parse_nonnegative_f32(value: &str) -> Result<f32, String> {
    let value = value
        .parse::<f32>()
        .map_err(|_| format!("expected a finite non-negative number, got {value:?}"))?;
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(format!(
            "expected a finite non-negative number, got {value}"
        ))
    }
}

struct EyeSource {
    map: MmapMut,
}

struct MutexGuard(*mut libc::pthread_mutex_t);

impl Drop for MutexGuard {
    fn drop(&mut self) {
        unsafe { libc::pthread_mutex_unlock(self.0) };
    }
}

struct EyeData {
    sample_time: f64,
    gaze: [[f32; 3]; 2],
    fixation_point: [f32; 3],
    openness: [f32; 2],
}

impl EyeSource {
    fn open() -> Result<Self, Box<dyn Error>> {
        let file = OpenOptions::new().read(true).write(true).open(SOURCE)?;
        if file.metadata()?.len() < SHM_SIZE as u64 {
            return Err(format!("{SOURCE}: shared memory is too small").into());
        }
        let map = unsafe { MmapOptions::new().len(SHM_SIZE).map_mut(&file)? };
        let source = Self { map };
        let layout = source.layout();
        let version = u32::from_le(unsafe { ptr::read_volatile(&raw const (*layout).version) });
        if version != SHM_VERSION {
            return Err(format!(
                "unsupported eye shared-memory version {version}; expected {SHM_VERSION} (Frame 0.5.0)"
            )
            .into());
        }
        if u32::from_le(unsafe { ptr::read_volatile(&raw const (*layout).initialized) }) != 1 {
            return Err("eye shared memory is not initialized".into());
        }
        Ok(source)
    }

    fn layout(&self) -> *const EyeServerMmap {
        self.map.as_ptr().cast()
    }

    fn layout_mut(&mut self) -> *mut EyeServerMmap {
        self.map.as_mut_ptr().cast()
    }

    fn lock(&mut self) -> io::Result<MutexGuard> {
        let mutex = unsafe { (&raw mut (*self.layout_mut()).metadata_mutex).cast() };
        let code = unsafe { libc::pthread_mutex_lock(mutex) };
        if code == libc::EOWNERDEAD {
            let result = unsafe { libc::pthread_mutex_consistent(mutex) };
            if result != 0 {
                unsafe { libc::pthread_mutex_unlock(mutex) };
                return Err(io::Error::from_raw_os_error(result));
            }
        } else if code != 0 {
            return Err(io::Error::from_raw_os_error(code));
        }
        Ok(MutexGuard(mutex))
    }

    fn next(&mut self, timeout: Duration) -> io::Result<Option<EyeData>> {
        let guard = self.lock()?;
        let sequence_ptr = unsafe { &raw const (*self.layout()).sequence };
        let sequence = unsafe { ptr::read_volatile(sequence_ptr) };
        let request_ptr = unsafe { &raw mut (*self.layout_mut()).metadata_requested };
        unsafe { ptr::write_volatile(request_ptr, 1) };
        drop(guard);

        let timespec = libc::timespec {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_nsec: timeout.subsec_nanos() as libc::c_long,
        };
        let result = unsafe {
            libc::syscall(
                libc::SYS_futex,
                sequence_ptr,
                libc::FUTEX_WAIT,
                sequence,
                &timespec as *const libc::timespec,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::EAGAIN | libc::EINTR | libc::ETIMEDOUT)
            ) {
                return Err(error);
            }
        }

        let guard = self.lock()?;
        let data = if unsafe { ptr::read_volatile(sequence_ptr) } != sequence {
            let record_ptr = unsafe { &raw const (*self.layout()).eye_data };
            let record = unsafe { ptr::read_unaligned(record_ptr) };
            if record.producer_state == 1 {
                Some(EyeData {
                    sample_time: record.sample_time,
                    gaze: record.gaze_direction,
                    fixation_point: record.fixation_point,
                    openness: record.openness,
                })
            } else {
                None
            }
        } else {
            None
        };
        drop(guard);
        Ok(data)
    }
}

fn send(socket: &UdpSocket, addr: String, args: Vec<OscType>) -> Result<(), Box<dyn Error>> {
    let packet = OscPacket::Message(OscMessage { addr, args });
    socket.send(&encoder::encode(&packet)?)?;
    Ok(())
}

// Like Steam Link's OSC sender, ±45° maps to ±1; +Y is up (VRCFT convention, unverified on hardware).
fn gaze_angles([x, y, z]: [f32; 3]) -> [f32; 2] {
    let scale = 4.0 / std::f32::consts::PI;
    [
        (x.atan2(-z) * scale).clamp(-1.0, 1.0),
        (y.atan2(-z) * scale).clamp(-1.0, 1.0),
    ]
}

fn normalized_eye_values(data: &EyeData) -> EyeValues {
    EyeValues {
        left: gaze_angles(data.gaze[0]),
        right: gaze_angles(data.gaze[1]),
        combined: gaze_angles(data.fixation_point),
        eyelids: [
            data.openness[0].clamp(0.0, 1.0),
            data.openness[1].clamp(0.0, 1.0),
        ],
    }
}

fn send_eye_data(socket: &UdpSocket, args: &Args, values: EyeValues) -> Result<(), Box<dyn Error>> {
    let prefix = format!("/avatar/parameters{}", args.prefix.trim_end_matches('/'));
    send(
        socket,
        format!("{prefix}/EyeTrackingActive"),
        vec![OscType::Bool(true)],
    )?;
    for (suffix, value) in [
        ("EyeLeftX", values.left[0]),
        ("EyeLeftY", values.left[1]),
        ("EyeRightX", values.right[0]),
        ("EyeRightY", values.right[1]),
        ("EyeLidLeft", values.eyelids[0]),
        ("EyeLidRight", values.eyelids[1]),
        ("EyeX", values.combined[0]),
        ("EyeY", values.combined[1]),
    ] {
        send(
            socket,
            format!("{prefix}/v2/{suffix}"),
            vec![OscType::Float(value)],
        )?;
    }
    Ok(())
}

fn send_inactive(socket: &UdpSocket, prefix: &str) -> Result<(), Box<dyn Error>> {
    send(
        socket,
        format!(
            "/avatar/parameters{}/EyeTrackingActive",
            prefix.trim_end_matches('/')
        ),
        vec![OscType::Bool(false)],
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if !args.prefix.starts_with('/') {
        return Err("--prefix must be a nonempty OSC path starting with /".into());
    }
    let target: SocketAddr = args
        .target
        .to_socket_addrs()?
        .next()
        .ok_or("--target did not resolve to an address")?;
    let socket = UdpSocket::bind(if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })?;
    socket.connect(target)?;
    let mut source = EyeSource::open()?;
    let mut eye_filter = OptionalEyeFilter::new(
        args.one_euro,
        OneEuroConfig {
            min_cutoff: args.one_euro_min_cutoff,
            beta: args.one_euro_beta,
            d_cutoff: args.one_euro_d_cutoff,
        },
    );
    eprintln!("Reading {SOURCE} and sending OSC to {target}");
    let mut active = false;
    loop {
        match source.next(TIMEOUT)? {
            Some(data)
                if data.sample_time.is_finite()
                    && data.gaze.iter().flatten().all(|value| value.is_finite())
                    && data.fixation_point.iter().all(|value| value.is_finite())
                    && data.openness.iter().all(|value| value.is_finite()) =>
            {
                let values = normalized_eye_values(&data);
                let values = eye_filter.filter(data.sample_time, values);
                send_eye_data(&socket, &args, values)?;
                active = true;
            }
            _ => {
                eye_filter.tracking_inactive();
                if active {
                    send_inactive(&socket, &args.prefix)?;
                    active = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_euro_cli_defaults_match_baballonia() {
        let args = Args::try_parse_from(["frameeyeosc", "--one-euro"]).unwrap();

        assert!(args.one_euro);
        assert_eq!(args.one_euro_min_cutoff, 0.5);
        assert_eq!(args.one_euro_beta, 3.0);
        assert_eq!(args.one_euro_d_cutoff, 1.0);
    }

    #[test]
    fn one_euro_cli_rejects_invalid_tuning_values() {
        for arguments in [
            ["--one-euro-min-cutoff", "0"],
            ["--one-euro-min-cutoff", "NaN"],
            ["--one-euro-beta", "-0.1"],
            ["--one-euro-beta", "inf"],
            ["--one-euro-d-cutoff", "0"],
            ["--one-euro-d-cutoff", "NaN"],
        ] {
            let result = Args::try_parse_from(["frameeyeosc", arguments[0], arguments[1]]);
            assert!(result.is_err(), "accepted invalid arguments: {arguments:?}");
        }
    }
}
