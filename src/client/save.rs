use std::{
    env, fmt,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const APP: &str = "8times8";
const CURRENT: &str = "save.dat";
const BACKUP: &str = "save.dat.bak";
const SESSION_LOCK: &str = "session.lock";
const MAGIC: &[u8; 8] = b"8times8\0";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 60;
const TRAILER_LEN: usize = 4;
const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
#[cfg(any(debug_assertions, test))]
const CLIPBOARD_PREFIX: &str = "8times8-save-v1:";
const DARK: u8 = 1;
const SOUND: u8 = 2;
const HINT_SEEN: u8 = 4;
const CAMERA: u8 = 8;
const SYSTEM_THEME: u8 = 16;

type Camera = ([i64; 2], [f64; 2], f64);

#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) struct Saved {
    pub board: Vec<u8>,
    pub machines: Vec<u8>,
    /// None follows the OS; Some(false) and Some(true) override it.
    pub dark: Option<bool>,
    pub sound: bool,
    pub hint_seen: bool,
    pub camera: Option<Camera>,
}

/// Keep the data directory open for one playing instance through its last save.
pub(super) struct SaveSession {
    _lock: File,
}

pub(super) fn lock_session() -> Result<SaveSession, String> {
    lock_session_at(&data_directory()?)
}

fn lock_session_at(dir: &Path) -> Result<SaveSession, String> {
    ensure_directory(dir).map_err(|e| at(dir, e))?;
    let path = dir.join(SESSION_LOCK);
    // Never remove the file: other processes must use this same lock identity.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| at(&path, e))?;
    match lock.try_lock() {
        Ok(()) => Ok(SaveSession { _lock: lock }),
        Err(TryLockError::WouldBlock) => Err(at(&path, "save is already open in another instance")),
        Err(TryLockError::Error(error)) => Err(at(&path, error)),
    }
}

pub(super) fn load_validated<T>(
    validate: impl FnMut(&Saved) -> Result<T, String>,
) -> Result<Option<(Saved, T)>, String> {
    load_at_checked(&data_directory()?, validate)
}

pub(super) fn store_validated(
    state: &Saved,
    validate: impl FnMut(&Saved) -> Result<(), String>,
) -> Result<(), String> {
    store_at_checked(&data_directory()?, state, validate)
}

/// Encode the native save container for debug copy/paste, not legacy JSON.
#[cfg(any(debug_assertions, test))]
pub(super) fn to_clipboard(state: &Saved) -> Result<String, String> {
    let header = encode_header(state).map_err(str::to_owned)?;
    let byte_len = HEADER_LEN + state.board.len() + state.machines.len() + TRAILER_LEN;
    let mut text = String::new();
    text.try_reserve_exact(CLIPBOARD_PREFIX.len() + byte_len * 2)
        .map_err(|err| format!("could not allocate save text: {err}"))?;
    text.push_str(CLIPBOARD_PREFIX);
    emit_generation(&header, state, |bytes| {
        append_hex(&mut text, bytes);
        Ok::<(), String>(())
    })?;
    Ok(text)
}

#[cfg(any(debug_assertions, test))]
fn append_hex(text: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 15)]));
    }
}

#[cfg(any(debug_assertions, test))]
pub(super) fn from_clipboard(text: &str) -> Result<Saved, String> {
    let hex = text
        .strip_prefix(CLIPBOARD_PREFIX)
        .ok_or("expected an 8times8-save-v1 native save; legacy JSON is not supported")?;
    let byte_len = clipboard_byte_len(hex.len())?;
    Generation::from_reader(HexReader(hex.as_bytes()), byte_len as u64)
        .and_then(Generation::into_saved)
        .map_err(|err| err.to_string())
}

#[cfg(any(debug_assertions, test))]
fn clipboard_byte_len(hex_len: usize) -> Result<usize, &'static str> {
    let overhead = HEADER_LEN + TRAILER_LEN;
    if hex_len > 2 * (overhead + MAX_PAYLOAD) {
        return Err("native save text exceeds the 64 MiB payload limit");
    }
    if hex_len < 2 * overhead || !hex_len.is_multiple_of(2) {
        return Err("invalid native save text length");
    }
    Ok(hex_len / 2)
}

#[cfg(any(debug_assertions, test))]
struct HexReader<'a>(&'a [u8]);

#[cfg(any(debug_assertions, test))]
impl Read for HexReader<'_> {
    fn read(&mut self, dest: &mut [u8]) -> io::Result<usize> {
        fn nibble(byte: u8) -> io::Result<u8> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "native save text is not lowercase hexadecimal",
                )),
            }
        }

        let n = dest.len().min(self.0.len() / 2);
        let (next, remaining) = self.0.split_at(n * 2);
        let (codes, _) = next.as_chunks::<2>();
        for (dest, code) in dest.iter_mut().zip(codes) {
            *dest = nibble(code[0])? << 4 | nibble(code[1])?;
        }
        self.0 = remaining;
        Ok(n)
    }
}

fn data_directory() -> Result<PathBuf, String> {
    fn absolute_env(key: &str) -> Option<PathBuf> {
        env::var_os(key)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }

    #[cfg(windows)]
    let base = absolute_env("LOCALAPPDATA")
        .or_else(|| absolute_env("USERPROFILE").map(|p| p.join("AppData").join("Local")));
    #[cfg(target_os = "macos")]
    let base = absolute_env("HOME").map(|p| p.join("Library").join("Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = absolute_env("XDG_DATA_HOME")
        .or_else(|| absolute_env("HOME").map(|p| p.join(".local").join("share")));

    base.map(|p| p.join(APP))
        .ok_or_else(|| format!("could not locate the {APP} user data directory"))
}

fn load_at_checked<T>(
    dir: &Path,
    mut validate: impl FnMut(&Saved) -> Result<T, String>,
) -> Result<Option<(Saved, T)>, String> {
    let current = dir.join(CURRENT);
    let backup = dir.join(BACKUP);
    let failure = match read_validated_generation(&current, &mut validate) {
        Ok(Some(state)) => return Ok(Some(state)),
        Ok(None) => None,
        // An older app must not roll a newer save back to an older backup.
        Err(error @ Fault::Version(_)) => return Err(at(&current, error)),
        Err(error) => Some(at(&current, error)),
    };
    match read_validated_generation(&backup, &mut validate) {
        Ok(Some(state)) => Ok(Some(state)),
        Ok(None) => failure.map_or(Ok(None), Err),
        Err(error) => match failure {
            Some(first) => Err(format!("{first}; {}", at(&backup, error))),
            None => Err(at(&backup, error)),
        },
    }
}

fn store_at_checked(
    dir: &Path,
    state: &Saved,
    mut validate: impl FnMut(&Saved) -> Result<(), String>,
) -> Result<(), String> {
    let header = encode_header(state).map_err(str::to_owned)?;
    validate(state).map_err(|error| format!("invalid new save payload: {error}"))?;
    ensure_directory(dir).map_err(|e| at(dir, e))?;
    let lock_path = dir.join("save.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| at(&lock_path, e))?;
    lock.lock().map_err(|e| at(&lock_path, e))?;

    let current = dir.join(CURRENT);
    let backup = dir.join(BACKUP);
    // Do not replace the last known good generation with an unverified one.
    let retire = match read_validated_generation(&current, &mut validate) {
        Ok(Some(_)) => {
            if let Err(error @ Fault::Version(_)) = Generation::open(&backup) {
                return Err(at(&backup, error));
            }
            Retire::Backup
        }
        Ok(None) => {
            read_validated_generation(&backup, &mut validate).map_err(|e| at(&backup, e))?;
            Retire::None
        }
        Err(error @ (Fault::Invalid(_) | Fault::Payload(_))) => {
            match read_validated_generation(&backup, &mut validate) {
                Ok(Some(_)) => Retire::Archive,
                Ok(None) => return Err(at(&current, error)),
                Err(other) => {
                    return Err(format!("{}; {}", at(&current, error), at(&backup, other)));
                }
            }
        }
        Err(error) => return Err(at(&current, error)),
    };

    let (staged, mut file) = Staged::create(dir, "tmp").map_err(|e| at(dir, e))?;
    let written = emit_generation(&header, state, |bytes| file.write_all(bytes))
        .and_then(|()| file.sync_all());
    drop(file);
    written.map_err(|e| at(&staged.path, e))?;

    match retire {
        Retire::Backup => {
            fs::rename(&current, &backup).map_err(|e| at(&backup, e))?;
            sync_directory(dir).map_err(|e| at(dir, e))?;
        }
        Retire::Archive => {
            let (mut archive, file) = Staged::create(dir, "corrupt").map_err(|e| at(dir, e))?;
            drop(file);
            fs::rename(&current, &archive.path).map_err(|e| at(&archive.path, e))?;
            // Keep the corrupt original for recovery and leave the good backup untouched.
            archive.cleanup = false;
            sync_directory(dir).map_err(|e| at(dir, e))?;
        }
        Retire::None => {}
    }
    // Between these renames, including after a crash, load can use the backup.
    fs::rename(&staged.path, &current).map_err(|e| at(&current, e))?;
    sync_directory(dir).map_err(|e| at(dir, e))
}

enum Retire {
    None,
    Backup,
    Archive,
}

fn ensure_directory(dir: &Path) -> io::Result<()> {
    if !dir.is_dir() {
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            ensure_directory(parent)?;
            fs::create_dir_all(dir)?;
            sync_directory(parent)?;
        } else {
            fs::create_dir_all(dir)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> io::Result<()> {
    // std does not expose a portable directory flush on Windows. The file is
    // flushed before being renamed, and both names live on the same volume.
    Ok(())
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct Staged {
    path: PathBuf,
    cleanup: bool,
}

impl Staged {
    fn create(dir: &Path, suffix: &str) -> io::Result<(Self, File)> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        for _ in 0..64 {
            let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{CURRENT}.{suffix}-{}-{n}", std::process::id()));
            match options.open(&path) {
                Ok(file) => {
                    return Ok((
                        Self {
                            path,
                            cleanup: true,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no unused staging filename",
        ))
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn at(path: &Path, error: impl fmt::Display) -> String {
    format!("{}: {error}", path.display())
}

// Version 1: magic, LE u16 version, settings flags, a reserved byte, LE u32
// lengths for board and machines, 40 bytes of camera fields (or zeros), the
// two opaque payloads in order, then a LE CRC-32 covering everything before it.
fn encode_header(state: &Saved) -> Result<[u8; HEADER_LEN], &'static str> {
    let board_len = state.board.len();
    let machines_len = state.machines.len();
    payload_size(board_len, machines_len).ok_or("save exceeds the 64 MiB payload limit")?;
    let mut hdr = [0; HEADER_LEN];
    hdr[..8].copy_from_slice(MAGIC);
    hdr[8..10].copy_from_slice(&VERSION.to_le_bytes());
    // The new flag represents system mode so unflagged draft saves still mean light.
    let theme = match state.dark {
        Some(true) => DARK,
        Some(false) => 0,
        None => SYSTEM_THEME,
    };
    hdr[10] = theme | (u8::from(state.sound) * SOUND) | (u8::from(state.hint_seen) * HINT_SEEN);
    hdr[12..16].copy_from_slice(&(board_len as u32).to_le_bytes());
    hdr[16..20].copy_from_slice(&(machines_len as u32).to_le_bytes());
    if let Some(camera @ (blocks, offsets, zoom)) = state.camera {
        if !valid_camera(camera) {
            return Err("invalid camera coordinates or zoom");
        }
        hdr[10] |= CAMERA;
        hdr[20..28].copy_from_slice(&blocks[0].to_le_bytes());
        hdr[28..36].copy_from_slice(&blocks[1].to_le_bytes());
        hdr[36..44].copy_from_slice(&offsets[0].to_le_bytes());
        hdr[44..52].copy_from_slice(&offsets[1].to_le_bytes());
        hdr[52..60].copy_from_slice(&zoom.to_le_bytes());
    }
    Ok(hdr)
}

fn valid_camera((_, offsets, zoom): Camera) -> bool {
    offsets.iter().all(|n| (0.0..8.0).contains(n)) && zoom.is_finite() && zoom > 0.0
}

fn emit_generation<E>(
    header: &[u8; HEADER_LEN],
    state: &Saved,
    mut emit: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let mut crc = Crc::new();
    for bytes in [&header[..], &state.board, &state.machines] {
        crc.update(bytes);
        emit(bytes)?;
    }
    emit(&crc.finish().to_le_bytes())
}

fn payload_size(board_len: usize, machines_len: usize) -> Option<usize> {
    board_len
        .checked_add(machines_len)
        .filter(|&n| n <= MAX_PAYLOAD)
}

enum Fault {
    Invalid(&'static str),
    Payload(String),
    Version(u16),
    Io(io::Error),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid save: {reason}"),
            Self::Payload(reason) => write!(f, "invalid save payload: {reason}"),
            Self::Version(version) => write!(f, "unsupported save version {version}"),
            Self::Io(e) => e.fmt(f),
        }
    }
}

impl From<io::Error> for Fault {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Self::Invalid("truncated file")
        } else {
            Self::Io(error)
        }
    }
}

struct Generation<R> {
    reader: R,
    crc: Crc,
    flags: u8,
    board_len: usize,
    machines_len: usize,
    camera: Option<Camera>,
}

impl Generation<File> {
    fn open(path: &Path) -> Result<Option<Self>, Fault> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(io::Error::other("save path is not a regular file").into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let total_len = file.metadata()?.len();
        Self::from_reader(file, total_len).map(Some)
    }
}

impl<R: Read> Generation<R> {
    fn from_reader(mut reader: R, total_len: u64) -> Result<Self, Fault> {
        let mut hdr = [0; HEADER_LEN];
        reader.read_exact(&mut hdr[..10])?;
        if &hdr[..8] != MAGIC {
            return Err(Fault::Invalid("wrong file signature"));
        }
        let version = u16::from_le_bytes(hdr[8..10].try_into().unwrap());
        if version != VERSION {
            // The prefix and trailing CRC are shared across container versions.
            // Protect a future save only after ruling out an altered version byte.
            verify_unknown_version(&mut reader, total_len, &hdr[..10])?;
            return Err(Fault::Version(version));
        }
        reader.read_exact(&mut hdr[10..])?;
        let flags = hdr[10];
        if flags & !(DARK | SOUND | HINT_SEEN | CAMERA | SYSTEM_THEME) != 0 || hdr[11] != 0 {
            return Err(Fault::Invalid("unknown settings flags"));
        }
        if flags & (DARK | SYSTEM_THEME) == (DARK | SYSTEM_THEME) {
            return Err(Fault::Invalid("conflicting theme flags"));
        }
        let board_len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
        let machines_len = u32::from_le_bytes(hdr[16..20].try_into().unwrap()) as usize;
        let payload_len = payload_size(board_len, machines_len)
            .ok_or(Fault::Invalid("payload exceeds the 64 MiB limit"))?;
        if total_len != (HEADER_LEN + payload_len + TRAILER_LEN) as u64 {
            return Err(Fault::Invalid(
                "container size does not match declared lengths",
            ));
        }
        let camera = if flags & CAMERA != 0 {
            let camera = (
                [20, 28].map(|i| i64::from_le_bytes(hdr[i..i + 8].try_into().unwrap())),
                [36, 44].map(|i| f64::from_le_bytes(hdr[i..i + 8].try_into().unwrap())),
                f64::from_le_bytes(hdr[52..60].try_into().unwrap()),
            );
            if !valid_camera(camera) {
                return Err(Fault::Invalid("invalid camera coordinates or zoom"));
            }
            Some(camera)
        } else {
            if hdr[20..] != [0; 40] {
                return Err(Fault::Invalid("camera data without camera flag"));
            }
            None
        };
        let mut crc = Crc::new();
        crc.update(&hdr);
        Ok(Self {
            reader,
            crc,
            flags,
            board_len,
            machines_len,
            camera,
        })
    }

    fn read_payload(&mut self, bytes: &mut [u8]) -> Result<(), Fault> {
        self.reader.read_exact(bytes)?;
        self.crc.update(bytes);
        Ok(())
    }

    fn into_saved(mut self) -> Result<Saved, Fault> {
        fn buffer(len: usize) -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(len)
                .map_err(|_| io::ErrorKind::OutOfMemory)?;
            bytes.resize(len, 0);
            Ok(bytes)
        }
        let mut board = buffer(self.board_len)?;
        let mut machines = buffer(self.machines_len)?;
        self.read_payload(&mut board)?;
        self.read_payload(&mut machines)?;
        check_trailer(&mut self.reader, &self.crc)?;
        Ok(Saved {
            board,
            machines,
            dark: (self.flags & SYSTEM_THEME == 0).then_some(self.flags & DARK != 0),
            sound: self.flags & SOUND != 0,
            hint_seen: self.flags & HINT_SEEN != 0,
            camera: self.camera,
        })
    }
}

fn read_validated_generation<T>(
    path: &Path,
    validate: &mut impl FnMut(&Saved) -> Result<T, String>,
) -> Result<Option<(Saved, T)>, Fault> {
    Generation::open(path)?
        .map(|generation| {
            let saved = generation.into_saved()?;
            let decoded = validate(&saved).map_err(Fault::Payload)?;
            Ok((saved, decoded))
        })
        .transpose()
}

fn verify_unknown_version(
    reader: &mut impl Read,
    total_len: u64,
    prefix: &[u8],
) -> Result<(), Fault> {
    if total_len > (HEADER_LEN + MAX_PAYLOAD + TRAILER_LEN) as u64 {
        return Err(Fault::Invalid("container exceeds the 64 MiB payload limit"));
    }
    let mut remaining = (total_len as usize)
        .checked_sub(prefix.len() + TRAILER_LEN)
        .ok_or(Fault::Invalid("truncated file"))?;
    let mut crc = Crc::new();
    crc.update(prefix);
    let mut chunk = [0; 64 * 1024];
    while remaining != 0 {
        let n = remaining.min(chunk.len());
        let chunk = &mut chunk[..n];
        reader.read_exact(chunk)?;
        crc.update(chunk);
        remaining -= n;
    }
    check_trailer(reader, &crc)
}

fn check_trailer(reader: &mut impl Read, crc: &Crc) -> Result<(), Fault> {
    let mut bytes = [0; TRAILER_LEN];
    reader.read_exact(&mut bytes)?;
    if crc.finish() != u32::from_le_bytes(bytes) {
        return Err(Fault::Invalid("checksum mismatch"));
    }
    if reader.read(&mut bytes[..1])? != 0 {
        return Err(Fault::Invalid("trailing bytes"));
    }
    Ok(())
}

struct Crc(u32);

impl Crc {
    const TABLE: [u32; 256] = {
        let mut table = [0; 256];
        let mut i = 0;
        while i < table.len() {
            let mut value = i as u32;
            let mut bits = 0;
            while bits < 8 {
                value = (value >> 1) ^ if value & 1 != 0 { 0xedb8_8320 } else { 0 };
                bits += 1;
            }
            table[i] = value;
            i += 1;
        }
        table
    };

    fn new() -> Self {
        Self(u32::MAX)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 >> 8) ^ Self::TABLE[((self.0 as u8) ^ byte) as usize];
        }
    }

    fn finish(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_at(dir: &Path) -> Result<Option<Saved>, String> {
        load_at_checked(dir, |_| Ok(())).map(|value| value.map(|(saved, ())| saved))
    }

    fn store_at(dir: &Path, state: &Saved) -> Result<(), String> {
        store_at_checked(dir, state, |_| Ok(()))
    }

    fn read_generation(path: &Path) -> Result<Option<Saved>, Fault> {
        read_validated_generation(path, &mut |_| Ok(())).map(|value| value.map(|(saved, ())| saved))
    }

    fn refresh_crc(bytes: &mut [u8]) {
        let (body, crc_bytes) = bytes.split_at_mut(bytes.len() - TRAILER_LEN);
        let mut crc = Crc::new();
        crc.update(body);
        crc_bytes.copy_from_slice(&crc.finish().to_le_bytes());
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let base = env::temp_dir();
            loop {
                let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let dir = base.join(format!("8times8-save-test-{}-{n}", std::process::id()));
                match fs::create_dir(&dir) {
                    Ok(()) => return Self(dir),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("{}", at(&dir, e)),
                }
            }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn saved(n: u8) -> Saved {
        Saved {
            board: vec![n; 4],
            machines: vec![n; 3],
            dark: match n % 3 {
                0 => None,
                1 => Some(false),
                _ => Some(true),
            },
            sound: n & 2 != 0,
            hint_seen: n & 4 != 0,
            camera: None,
        }
    }

    #[test]
    fn settings_and_full_coordinate_precision_round_trip() {
        let root = TestDir::new();
        let dir = root.0.join("fresh").join(APP);
        assert_eq!(load_at(&dir).unwrap(), None);
        assert!(!dir.exists());

        for n in 0..8 {
            let mut state = saved(n);
            state.camera = Some(([i64::MIN, i64::MAX], [-0.0, 8.0f64.next_down()], 97.0 / 3.0));
            store_at(&dir, &state).unwrap();
            let actual = load_at(&dir).unwrap().unwrap();
            assert_eq!(actual, state);
            assert_eq!(actual.camera.unwrap().1[0].to_bits(), (-0.0_f64).to_bits());
        }
        let no_camera = saved(7);
        store_at(&dir, &no_camera).unwrap();
        assert_eq!(load_at(&dir).unwrap(), Some(no_camera));
    }

    #[test]
    fn corrupt_current_falls_back_and_is_preserved_on_resave() {
        let TestDir(ref dir) = TestDir::new();
        let current = dir.join(CURRENT);
        let backup = dir.join(BACKUP);
        store_at(dir, &saved(1)).unwrap();
        store_at(dir, &saved(2)).unwrap();
        let prior = fs::read(backup).unwrap();
        let mut damaged = fs::read(&current).unwrap();
        damaged[HEADER_LEN] ^= 0x01;
        fs::write(&current, &damaged).unwrap();

        assert_eq!(load_at(dir).unwrap(), Some(saved(1)));
        assert_eq!(fs::read(&current).unwrap(), damaged);
        store_at(dir, &saved(3)).unwrap();
        assert_eq!(load_at(dir).unwrap(), Some(saved(3)));
        assert_eq!(fs::read(dir.join(BACKUP)).unwrap(), prior);
        let archives: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("save.dat.corrupt-")
            })
            .collect();
        assert_eq!(archives.len(), 1);
        assert_eq!(fs::read(&archives[0]).unwrap(), damaged);
    }

    #[test]
    fn unrecoverable_damage_or_newer_version_is_not_overwritten() {
        let TestDir(ref dir) = TestDir::new();
        let current = dir.join(CURRENT);
        let backup = dir.join(BACKUP);
        store_at(dir, &saved(1)).unwrap();
        store_at(dir, &saved(2)).unwrap();
        let mut future = fs::read(&current).unwrap();
        future[8..10].copy_from_slice(&(VERSION + 1).to_le_bytes());
        refresh_crc(&mut future);
        fs::write(&current, &future).unwrap();
        assert!(
            load_at(dir)
                .unwrap_err()
                .contains("unsupported save version")
        );
        assert!(store_at(dir, &saved(3)).is_err());
        assert_eq!(fs::read(&current).unwrap(), future);

        fs::write(&current, b"partial").unwrap();
        fs::write(&backup, b"corrupt").unwrap();
        assert!(load_at(dir).is_err());
        assert!(store_at(dir, &saved(4)).is_err());
        assert_eq!(fs::read(&current).unwrap(), b"partial");
        assert_eq!(fs::read(&backup).unwrap(), b"corrupt");
    }

    #[test]
    fn corrupt_version_byte_falls_back_and_is_preserved_on_resave() {
        let TestDir(ref dir) = TestDir::new();
        let current = dir.join(CURRENT);
        store_at(dir, &saved(1)).unwrap();
        store_at(dir, &saved(2)).unwrap();

        let mut damaged = fs::read(&current).unwrap();
        damaged[8] ^= 0x02;
        fs::write(&current, &damaged).unwrap();
        assert_eq!(load_at(dir).unwrap(), Some(saved(1)));
        assert_eq!(fs::read(&current).unwrap(), damaged);
        store_at(dir, &saved(3)).unwrap();
        assert_eq!(load_at(dir).unwrap(), Some(saved(3)));
        assert_eq!(
            read_generation(&dir.join(BACKUP)).ok().flatten(),
            Some(saved(1))
        );
        assert_archived(dir, &damaged);
    }

    #[test]
    fn component_decode_failure_with_valid_crc_does_not_displace_good_backup() {
        fn validate(state: &Saved) -> Result<(), String> {
            if state.board.contains(&u8::MAX) || state.machines.contains(&u8::MAX) {
                return Err("invalid component bytes".into());
            }
            Ok(())
        }

        for corrupt_board in [true, false] {
            let TestDir(ref dir) = TestDir::new();
            let current = dir.join(CURRENT);
            let backup = dir.join(BACKUP);
            let mut invalid = saved(2);
            if corrupt_board {
                invalid.board[0] = u8::MAX;
            } else {
                invalid.machines[0] = u8::MAX;
            }
            store_at(dir, &saved(1)).unwrap();
            store_at(dir, &invalid).unwrap(); // A correct envelope with an invalid component.
            assert_eq!(load_at(dir).unwrap(), Some(invalid));

            let prior = fs::read(&backup).unwrap();
            let damaged = fs::read(&current).unwrap();
            assert_eq!(
                load_at_checked(dir, validate).unwrap(),
                Some((saved(1), ()))
            );
            assert_eq!(fs::read(&current).unwrap(), damaged);
            store_at_checked(dir, &saved(3), validate).unwrap();
            assert_eq!(
                load_at_checked(dir, validate).unwrap(),
                Some((saved(3), ()))
            );
            assert_eq!(fs::read(&backup).unwrap(), prior);
            assert_archived(dir, &damaged);
        }
    }

    fn assert_archived(dir: &Path, bytes: &[u8]) {
        let archive = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("save.dat.corrupt-")
            })
            .unwrap();
        assert_eq!(fs::read(archive).unwrap(), bytes);
    }

    #[test]
    fn interrupted_publication_leaves_a_loadable_backup() {
        let TestDir(ref dir) = TestDir::new();
        store_at(dir, &saved(1)).unwrap();
        fs::rename(dir.join(CURRENT), dir.join(BACKUP)).unwrap();
        fs::write(dir.join("save.dat.tmp-interrupted"), b"unfinished").unwrap();
        assert_eq!(load_at(dir).unwrap(), Some(saved(1)));
        store_at(dir, &saved(2)).unwrap();
        assert_eq!(load_at(dir).unwrap(), Some(saved(2)));
        assert_eq!(
            read_generation(&dir.join(BACKUP)).ok().flatten(),
            Some(saved(1))
        );
    }

    #[test]
    fn handles_large_sections_and_rejects_invalid_lengths_before_loading() {
        let TestDir(ref dir) = TestDir::new();
        let state = Saved {
            board: vec![0xfe; 16 * 1024 * 1024 + 1],
            ..saved(7)
        };
        store_at(dir, &state).unwrap();
        assert_eq!(load_at(dir).unwrap(), Some(state));

        let current = dir.join(CURRENT);
        let mut hdr = encode_header(&saved(0)).unwrap();
        hdr[12..16].copy_from_slice(&(MAX_PAYLOAD as u32).to_le_bytes());
        fs::write(&current, hdr).unwrap();
        assert!(load_at(dir).unwrap_err().contains("64 MiB"));
        assert!(store_at(dir, &saved(2)).is_err());
        assert_eq!(fs::read(&current).unwrap(), hdr);
    }

    #[test]
    fn invalid_camera_and_failed_rotation_preserve_current() {
        let TestDir(ref dir) = TestDir::new();
        store_at(dir, &saved(1)).unwrap();
        for camera in [([0, 0], [0.0, 8.0], 1.0), ([0, 0], [0.0, 0.0], f64::NAN)] {
            assert!(
                store_at(
                    dir,
                    &Saved {
                        camera: Some(camera),
                        ..saved(2)
                    }
                )
                .is_err()
            );
            assert_eq!(load_at(dir).unwrap(), Some(saved(1)));
        }

        fs::create_dir(dir.join(BACKUP)).unwrap();
        fs::write(dir.join(BACKUP).join("not-a-save"), []).unwrap();
        assert!(store_at(dir, &saved(2)).is_err());
        assert_eq!(load_at(dir).unwrap(), Some(saved(1)));
        assert_eq!(fs::read_dir(dir).unwrap().count(), 3); // current, backup dir, lock
    }

    #[test]
    fn concurrent_writers_leave_two_complete_generations() {
        let TestDir(ref dir) = TestDir::new();
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|threads| {
            for n in [1, 2] {
                let start = &start;
                threads.spawn(move || {
                    start.wait();
                    store_at(dir, &saved(n)).unwrap();
                });
            }
        });
        let current = load_at(dir).unwrap().unwrap();
        let previous = read_generation(&dir.join(BACKUP)).ok().flatten().unwrap();
        let mut states = [current, previous];
        states.sort_by_key(|state| state.board[0]);
        assert_eq!(states, [saved(1), saved(2)]);
    }

    #[test]
    fn session_lock_excludes_a_second_instance_and_releases_on_drop() {
        let root = TestDir::new();
        let dir = root.0.join("fresh").join(APP);
        let lock = lock_session_at(&dir).unwrap();
        let Err(error) = lock_session_at(&dir) else {
            panic!("a second instance acquired the session lock");
        };
        assert!(error.contains("save is already open in another instance"));
        // The session lease does not conflict with the separate writer lock.
        store_at(&dir, &saved(1)).unwrap();
        drop(lock);
        assert!(dir.join(SESSION_LOCK).is_file());

        let new_lock = lock_session_at(&dir).unwrap();
        assert_eq!(load_at(&dir).unwrap(), Some(saved(1)));
        drop(new_lock);
        assert!(dir.join(SESSION_LOCK).is_file());
    }

    #[test]
    fn session_lock_distinguishes_contention_from_other_io_errors() {
        let TestDir(ref dir) = TestDir::new();
        fs::create_dir(dir.join(SESSION_LOCK)).unwrap();
        let Err(error) = lock_session_at(dir) else {
            panic!("a directory was accepted as a session lock file");
        };
        assert!(error.contains(SESSION_LOCK));
        assert!(!error.contains("already open"));
    }

    #[test]
    fn clipboard_round_trip_uses_the_exact_native_container() {
        let TestDir(ref dir) = TestDir::new();
        let state = Saved {
            board: (0..=255).collect(),
            camera: Some(([i64::MIN, i64::MAX], [-0.0, 8.0f64.next_down()], 97.0 / 3.0)),
            ..saved(7)
        };
        store_at(dir, &state).unwrap();
        let text = to_clipboard(&state).unwrap();
        let hex = text.strip_prefix(CLIPBOARD_PREFIX).unwrap();
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let mut bytes = Vec::new();
        HexReader(hex.as_bytes()).read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, fs::read(dir.join(CURRENT)).unwrap());
        let decoded = from_clipboard(&text).unwrap();
        assert_eq!(decoded, state);
        assert_eq!(decoded.camera.unwrap().1[0].to_bits(), (-0.0_f64).to_bits());
    }

    #[test]
    fn clipboard_rejects_legacy_truncated_nonhex_and_tampered_data() {
        let text = to_clipboard(&saved(5)).unwrap();
        assert!(
            from_clipboard(r#"{"v":2,"blocks":[]}"#)
                .unwrap_err()
                .contains("legacy JSON")
        );
        assert!(from_clipboard(&text.replacen("-v1:", "-v2:", 1)).is_err());
        assert!(from_clipboard(CLIPBOARD_PREFIX).is_err());
        assert!(from_clipboard(&text[..text.len() - 1]).is_err());
        assert!(from_clipboard(&text[..text.len() - 2]).is_err());
        assert!(from_clipboard(&format!("{text}00")).is_err());

        let payload_at = CLIPBOARD_PREFIX.len() + HEADER_LEN * 2;
        let mut bad_hex = text.clone();
        bad_hex.replace_range(payload_at..payload_at + 2, "é");
        assert!(
            from_clipboard(&bad_hex)
                .unwrap_err()
                .contains("lowercase hexadecimal")
        );
        bad_hex.replace_range(payload_at..payload_at + 2, "5A");
        assert!(from_clipboard(&bad_hex).is_err());
        bad_hex.replace_range(payload_at..payload_at + 2, "06");
        assert!(
            from_clipboard(&bad_hex)
                .unwrap_err()
                .contains("checksum mismatch")
        );
    }

    #[test]
    fn clipboard_checks_limits_and_internal_version() {
        let max_encoded = 2 * (HEADER_LEN + MAX_PAYLOAD + TRAILER_LEN);
        assert_eq!(clipboard_byte_len(max_encoded), Ok(max_encoded / 2));
        assert!(clipboard_byte_len(max_encoded + 2).is_err());
        assert!(clipboard_byte_len(usize::MAX).is_err());

        let text = to_clipboard(&saved(5)).unwrap();
        let mut text_with_oversize_header = text.clone();
        let lengths_at = CLIPBOARD_PREFIX.len() + 12 * 2;
        text_with_oversize_header.replace_range(lengths_at..lengths_at + 8, "00000004");
        assert!(
            from_clipboard(&text_with_oversize_header)
                .unwrap_err()
                .contains("64 MiB")
        );
        let mut future = text;
        let version_at = CLIPBOARD_PREFIX.len() + 8 * 2;
        future.replace_range(version_at..version_at + 4, "0200");
        assert!(
            from_clipboard(&future)
                .unwrap_err()
                .contains("checksum mismatch")
        );
        let mut raw = Vec::new();
        let future_hex = future.strip_prefix(CLIPBOARD_PREFIX).unwrap();
        HexReader(future_hex.as_bytes())
            .read_to_end(&mut raw)
            .unwrap();
        refresh_crc(&mut raw);
        future.truncate(CLIPBOARD_PREFIX.len());
        append_hex(&mut future, &raw);
        assert!(
            from_clipboard(&future)
                .unwrap_err()
                .contains("unsupported save version 2")
        );
    }

    #[test]
    fn clipboard_preserves_theme_preference_and_rejects_invalid_theme_bits() {
        let flags_at = CLIPBOARD_PREFIX.len() + 10 * 2;
        for (dark, flags) in [(None, SYSTEM_THEME), (Some(false), 0), (Some(true), DARK)] {
            let state = Saved { dark, ..saved(0) };
            let text = to_clipboard(&state).unwrap();
            assert_eq!(&text[flags_at..flags_at + 2], format!("{flags:02x}"));
            assert_eq!(from_clipboard(&text).unwrap(), state);
        }

        let text = to_clipboard(&saved(0)).unwrap();
        for (flags, error) in [
            ("11", "conflicting theme flags"),
            ("80", "unknown settings flags"),
        ] {
            let mut invalid = text.clone();
            invalid.replace_range(flags_at..flags_at + 2, flags);
            assert!(from_clipboard(&invalid).unwrap_err().contains(error));
        }
    }

    #[test]
    fn crc32_matches_standard_check_value() {
        let mut crc = Crc::new();
        crc.update(b"1234");
        crc.update(b"56789");
        assert_eq!(crc.finish(), 0xcbf4_3926);
    }
}
