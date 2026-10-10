use clap::ValueEnum;
use memmap2::{MmapMut, MmapOptions};
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    ptr,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ShmVersion {
    Auto,
    #[value(name = "4")]
    V4,
    #[value(name = "5")]
    V5,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputStatus {
    Ready,
    Unavailable,
    AbiMismatch,
    InvalidSample,
}
#[derive(Debug)]
pub(crate) struct EyeData {
    pub sample_time: f64,
    pub gaze: [[f32; 3]; 2],
    pub fixation_point: [f32; 3],
    pub openness: [f32; 2],
}
#[derive(Debug)]
pub(crate) enum EyeUpdate {
    Data(EyeData),
    Inactive,
    NoUpdate,
    Invalid,
}
fn layout(selected: ShmVersion, version: u32, size: usize) -> io::Result<usize> {
    let (required, offset) = match version {
        4 if selected != ShmVersion::V5 => (0x4f21a, 0x152),
        5 if selected != ShmVersion::V4 => (0x4f21f, 0x157),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported ABI",
            ));
        }
    };
    if size < required {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shared memory too small",
        ));
    }
    Ok(offset)
}
fn decode(bytes: &[u8]) -> EyeUpdate {
    if u32::from_le_bytes(bytes[..4].try_into().unwrap()) != 1 {
        return EyeUpdate::Inactive;
    }
    let f = |offset| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let data = EyeData {
        sample_time: f64::from_le_bytes(bytes[5..13].try_into().unwrap()),
        gaze: std::array::from_fn(|eye| std::array::from_fn(|axis| f(0x0d + eye * 12 + axis * 4))),
        fixation_point: std::array::from_fn(|axis| f(0x3d + axis * 4)),
        openness: [f(0x79), f(0x7d)],
    };
    if !data.sample_time.is_finite()
        || !data
            .gaze
            .iter()
            .flatten()
            .chain(data.fixation_point.iter())
            .chain(data.openness.iter())
            .all(|x| x.is_finite())
    {
        EyeUpdate::Invalid
    } else {
        EyeUpdate::Data(data)
    }
}
pub(crate) struct EyeSource {
    map: MmapMut,
    file: File,
    path: PathBuf,
    version: ShmVersion,
    identity: (u64, u64, u64),
    sequence: u32,
    last_update: Instant,
}
struct Guard(*mut libc::pthread_mutex_t);
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_mutex_unlock(self.0);
        }
    }
}
impl EyeSource {
    pub(crate) fn open(path: &Path, version: ShmVersion) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let meta = file.metadata()?;
        if meta.len() < 8 {
            return Err(io::Error::new(io::ErrorKind::NotFound, "short header"));
        }
        // External truncation between checks remains a platform-level mmap hazard.
        let map = unsafe {
            MmapOptions::new()
                .len((meta.len() as usize).min(0x4f21f))
                .map_mut(&file)?
        };
        let mut source = Self {
            map,
            file,
            path: path.into(),
            version,
            identity: (meta.dev(), meta.ino(), meta.len()),
            sequence: 0,
            last_update: Instant::now(),
        };
        source.validate()?;
        source.sequence = source.read_u32(0x38);
        Ok(source)
    }
    fn read_u32(&self, offset: usize) -> u32 {
        u32::from_le(unsafe { ptr::read_volatile(self.map.as_ptr().add(offset).cast::<u32>()) })
    }
    pub(crate) fn validate(&self) -> io::Result<usize> {
        let metadata = self.file.metadata()?;
        let path = std::fs::metadata(&self.path)?;
        if (path.dev(), path.ino(), path.len()) != self.identity
            || metadata.len() != self.identity.2
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "input replaced or resized",
            ));
        }
        self.validate_header()
    }
    fn validate_header(&self) -> io::Result<usize> {
        if self.read_u32(4) != 1 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "input not initialized",
            ));
        }
        if self.read_u32(0) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "input version not initialized",
            ));
        }
        layout(self.version, self.read_u32(0), self.identity.2 as usize)
    }
    pub(crate) fn next(&mut self, budget: Duration) -> io::Result<EyeUpdate> {
        let deadline = Instant::now() + budget.min(Duration::from_millis(10));
        self.validate()?;
        let mut waited = false;
        loop {
            let mutex = unsafe { self.map.as_mut_ptr().add(8).cast::<libc::pthread_mutex_t>() };
            let guard = loop {
                let result = unsafe { libc::pthread_mutex_trylock(mutex) };
                if result == 0 {
                    break Guard(mutex);
                }
                if result == libc::EOWNERDEAD {
                    let guard = Guard(mutex);
                    let result = unsafe { libc::pthread_mutex_consistent(mutex) };
                    if result != 0 {
                        return Err(io::Error::from_raw_os_error(result));
                    }
                    self.validate_header()?;
                    // The sequence may describe a partially written record; discard it forever.
                    self.sequence = self.read_u32(0x38);
                    unsafe {
                        ptr::write_volatile(
                            self.map.as_mut_ptr().add(0x3c).cast::<u32>(),
                            1u32.to_le(),
                        );
                    }
                    drop(guard);
                    return Ok(self.empty());
                }
                if result != libc::EBUSY {
                    return Err(io::Error::from_raw_os_error(result));
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(self.empty());
                }
                std::thread::sleep(remaining.min(Duration::from_micros(100)));
            };
            let offset = self.validate_header()?;
            unsafe {
                ptr::write_volatile(self.map.as_mut_ptr().add(0x3c).cast::<u32>(), 1u32.to_le());
            }
            let sequence = self.read_u32(0x38);
            if sequence != self.sequence {
                let mut bytes = [0u8; 0x81];
                for (index, byte) in bytes.iter_mut().enumerate() {
                    *byte = unsafe { ptr::read_volatile(self.map.as_ptr().add(offset + index)) };
                }
                self.sequence = sequence;
                self.last_update = Instant::now();
                drop(guard);
                self.validate()?;
                return Ok(decode(&bytes));
            }
            drop(guard);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if waited || remaining.is_zero() {
                return Ok(self.empty());
            }
            let timeout = libc::timespec {
                tv_sec: remaining.as_secs() as libc::time_t,
                tv_nsec: remaining.subsec_nanos() as libc::c_long,
            };
            let result = unsafe {
                libc::syscall(
                    libc::SYS_futex,
                    self.map.as_ptr().add(0x38).cast::<u32>(),
                    libc::FUTEX_WAIT,
                    sequence.to_le(),
                    &timeout as *const libc::timespec,
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
            waited = true;
            // Recheck pathname/size before touching a mapping after the wait.
            self.validate()?;
        }
    }
    fn empty(&self) -> EyeUpdate {
        if self.last_update.elapsed() >= Duration::from_secs(1) {
            EyeUpdate::Inactive
        } else {
            EyeUpdate::NoUpdate
        }
    }
}
const _: () = {
    assert!(std::mem::size_of::<libc::pthread_mutex_t>() <= 48);
    assert!(8 % std::mem::align_of::<libc::pthread_mutex_t>() == 0);
};
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_layouts_and_mismatch() {
        for (version, offset, size) in [(4, 0x152, 0x4f21a), (5, 0x157, 0x4f21f)] {
            assert_eq!(layout(ShmVersion::Auto, version, size).unwrap(), offset);
            assert!(layout(ShmVersion::Auto, version, size - 1).is_err());
        }
        assert!(layout(ShmVersion::V4, 5, 0x4f21f).is_err());
        assert!(layout(ShmVersion::Auto, 6, 0x4f21f).is_err());
    }
    #[test]
    fn zero_valid_and_one_nonfinite_invalid() {
        let mut bytes = vec![0u8; 0x81];
        bytes[..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(decode(&bytes), EyeUpdate::Data(_)));
        bytes[0x7d..0x81].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(matches!(decode(&bytes), EyeUpdate::Invalid));
    }
}
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use std::{
        os::unix::fs::FileExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    pub struct Fixture {
        pub dir: PathBuf,
        pub path: PathBuf,
        pub file: File,
        pub map: MmapMut,
        pub offset: usize,
    }
    impl Fixture {
        pub fn new(version: u32) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "frameeyeosc-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            let path = dir.join("source");
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            let size = if version == 4 { 0x4f21a } else { 0x4f21f };
            file.set_len(size).unwrap();
            let mut map = unsafe { MmapOptions::new().map_mut(&file).unwrap() };
            map[..4].copy_from_slice(&version.to_le_bytes());
            map[4..8].copy_from_slice(&1u32.to_le_bytes());
            let mut attrs = std::mem::MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
            unsafe {
                assert_eq!(libc::pthread_mutexattr_init(attrs.as_mut_ptr()), 0);
                assert_eq!(
                    libc::pthread_mutexattr_setpshared(
                        attrs.as_mut_ptr(),
                        libc::PTHREAD_PROCESS_SHARED
                    ),
                    0
                );
                assert_eq!(
                    libc::pthread_mutexattr_setrobust(
                        attrs.as_mut_ptr(),
                        libc::PTHREAD_MUTEX_ROBUST
                    ),
                    0
                );
                assert_eq!(
                    libc::pthread_mutex_init(map.as_mut_ptr().add(8).cast(), attrs.as_ptr()),
                    0
                );
                assert_eq!(libc::pthread_mutexattr_destroy(attrs.as_mut_ptr()), 0);
            }
            Self {
                dir,
                path,
                file,
                map,
                offset: if version == 4 { 0x152 } else { 0x157 },
            }
        }
        pub fn write(&self, offset: u64, bytes: &[u8]) {
            self.file.write_all_at(bytes, offset).unwrap();
        }
        pub fn update(&self, sequence: u32, time: f64, openness: [f32; 2]) {
            self.write(self.offset as u64, &1u32.to_le_bytes());
            self.write((self.offset + 5) as u64, &time.to_le_bytes());
            for (index, value) in openness.into_iter().enumerate() {
                self.write(
                    (self.offset + 0x79 + index * 4) as u64,
                    &value.to_le_bytes(),
                );
            }
            self.write(0x38, &sequence.to_le_bytes());
        }
        pub fn mutex(&mut self) -> *mut libc::pthread_mutex_t {
            unsafe { self.map.as_mut_ptr().add(8).cast() }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).unwrap();
        }
    }
}
#[cfg(test)]
mod lifecycle_tests {
    use super::fixtures::Fixture;
    use super::*;
    #[test]
    fn real_packed_samples_and_header_revalidation() {
        for version in [4, 5] {
            let fixture = Fixture::new(version);
            let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
            fixture.update(1, 123.5, [0.0, 0.7]);
            let EyeUpdate::Data(data) = source.next(Duration::ZERO).unwrap() else {
                panic!("expected packed sample")
            };
            assert_eq!(data.sample_time, 123.5);
            assert_eq!(data.openness, [0.0, 0.7]);
            assert!(matches!(
                source.next(Duration::ZERO).unwrap(),
                EyeUpdate::NoUpdate
            ));
            fixture.write(4, &0u32.to_le_bytes());
            assert_eq!(
                source.next(Duration::ZERO).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
            fixture.write(4, &1u32.to_le_bytes());
            fixture.write(0, &9u32.to_le_bytes());
            assert_eq!(
                source.next(Duration::ZERO).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
    #[test]
    fn deletion_resize_recreation() {
        let fixture = Fixture::new(4);
        let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
        fixture.file.set_len(8).unwrap();
        assert!(source.next(Duration::ZERO).is_err());
        std::fs::remove_file(&fixture.path).unwrap();
        assert!(source.next(Duration::ZERO).is_err());
        std::fs::write(&fixture.path, [0; 8]).unwrap();
        assert!(source.next(Duration::ZERO).is_err());
    }
    #[test]
    fn busy_is_bounded_and_does_not_confirm_10ms_inactivity() {
        let mut fixture = Fixture::new(4);
        let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
        let mutex = fixture.mutex();
        assert_eq!(unsafe { libc::pthread_mutex_lock(mutex) }, 0);
        let start = Instant::now();
        assert!(matches!(
            source.next(Duration::from_millis(3)).unwrap(),
            EyeUpdate::NoUpdate
        ));
        assert!(start.elapsed() < Duration::from_millis(100));
        source.last_update = Instant::now() - Duration::from_secs(2);
        assert!(matches!(
            source.next(Duration::ZERO).unwrap(),
            EyeUpdate::Inactive
        ));
        assert_eq!(unsafe { libc::pthread_mutex_unlock(mutex) }, 0);
    }
    #[test]
    fn owner_death_is_made_consistent_before_unlock() {
        let mut fixture = Fixture::new(4);
        let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
        let address = fixture.mutex() as usize;
        std::thread::spawn(move || {
            assert_eq!(
                unsafe { libc::pthread_mutex_lock(address as *mut libc::pthread_mutex_t) },
                0
            );
        })
        .join()
        .unwrap();
        assert!(matches!(
            source.next(Duration::ZERO).unwrap(),
            EyeUpdate::NoUpdate
        ));
        assert_eq!(unsafe { libc::pthread_mutex_trylock(fixture.mutex()) }, 0);
        assert_eq!(unsafe { libc::pthread_mutex_unlock(fixture.mutex()) }, 0);
    }
}
#[cfg(test)]
mod review_tests {
    use super::fixtures::Fixture;
    use super::*;
    #[test]
    fn startup_unprepared_is_unavailable() {
        for size in [0, 7, 0x4f21a] {
            let fixture = Fixture::new(4);
            fixture.file.set_len(size).unwrap();
            if size >= 8 {
                fixture.write(0, &0u32.to_le_bytes());
                fixture.write(4, &0u32.to_le_bytes());
            }
            assert_eq!(
                EyeSource::open(&fixture.path, ShmVersion::Auto)
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::NotFound
            );
        }
    }
    #[test]
    fn finite_out_of_range_remains_raw_data() {
        let fixture = Fixture::new(4);
        let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
        fixture.update(1, 1.0, [1.0001, -0.0001]);
        let EyeUpdate::Data(data) = source.next(Duration::ZERO).unwrap() else {
            panic!("finite raw openness must survive")
        };
        assert_eq!(crate::normalized_eye_values(&data).eyelids, [1.0, 0.0]);
    }
    #[test]
    fn owner_death_discards_partial_sequence() {
        let mut fixture = Fixture::new(4);
        let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
        fixture.update(1, 1.0, [0.7; 2]);
        let address = fixture.mutex() as usize;
        std::thread::spawn(move || {
            assert_eq!(
                unsafe { libc::pthread_mutex_lock(address as *mut libc::pthread_mutex_t) },
                0
            );
        })
        .join()
        .unwrap();
        assert!(matches!(
            source.next(Duration::ZERO).unwrap(),
            EyeUpdate::NoUpdate
        ));
        assert!(matches!(
            source.next(Duration::ZERO).unwrap(),
            EyeUpdate::NoUpdate
        ));
        fixture.update(2, 2.0, [0.8; 2]);
        assert!(matches!(
            source.next(Duration::ZERO).unwrap(),
            EyeUpdate::Data(_)
        ));
    }
    #[test]
    fn idle_call_waits_for_futex_budget() {
        let fixture = Fixture::new(4);
        let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
        let start = Instant::now();
        assert!(matches!(
            source.next(Duration::from_millis(5)).unwrap(),
            EyeUpdate::NoUpdate
        ));
        assert!(start.elapsed() >= Duration::from_millis(4));
    }
    #[test]
    fn mutex_busy_large_budget_still_returns_within_10ms_cap() {
        let mut fixture = Fixture::new(4);
        let mut source = EyeSource::open(&fixture.path, ShmVersion::Auto).unwrap();
        assert_eq!(unsafe { libc::pthread_mutex_lock(fixture.mutex()) }, 0);
        let start = Instant::now();
        let update = source.next(Duration::from_secs(1)).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(unsafe { libc::pthread_mutex_unlock(fixture.mutex()) }, 0);
        assert!(matches!(update, EyeUpdate::NoUpdate));
        assert!(elapsed < Duration::from_millis(50), "{elapsed:?}");
    }
}
