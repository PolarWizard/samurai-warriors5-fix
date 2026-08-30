use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use grapnel::{Context, MidHook};
use windows::Win32::Foundation::{FILETIME, HMODULE, SYSTEMTIME};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect};
use windows::Win32::System::ProcessStatus::{GetModuleInformation, MODULEINFO};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::Time::FileTimeToSystemTime;

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// Opens (truncating) the log file at `path` for [`log`] to write into.
pub fn init_log_file(path: &str) {
    if let Ok(file) = File::create(path) {
        let _ = LOG_FILE.set(Mutex::new(file));
    }
}

/// A severity tag written into each log line.
#[derive(Clone, Copy)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
    Debug,
}

impl LogLevel {
    fn tag(self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warning => "WARNING",
            LogLevel::Error => "ERROR",
            LogLevel::Debug => "DEBUG",
        }
    }
}

/// Formats a broken-down time: `YYYY-MM-DDTHH:MM:SS.mmm`
fn format_systemtime(t: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

/// Appends a timestamped, level-tagged line to the file [`init_log_file`]
/// opened, if one has been opened yet.
fn log_at(level: LogLevel, message: &str) {
    let Some(file) = LOG_FILE.get() else { return };
    let Ok(mut file) = file.lock() else { return };
    let t = unsafe { GetLocalTime() };
    let _ = writeln!(file, "[{}] [{}] {message}", format_systemtime(&t), level.tag());
}

/// Logs at [`LogLevel::Info`] -- the default for a normal-operation line
/// (a fix applied, a signature matched, a config value in effect).
pub fn log(message: &str) {
    log_at(LogLevel::Info, message);
}

/// Logs at [`LogLevel::Warning`] -- a fallback path was taken but the mod
/// still functions (e.g. a malformed config value, falling back to a default).
pub fn log_warn(message: &str) {
    log_at(LogLevel::Warning, message);
}

/// Logs at [`LogLevel::Error`] -- a fix did not apply (signature not found,
/// hook install failed) or the process panicked.
pub fn log_error(message: &str) {
    log_at(LogLevel::Error, message);
}

/// Logs at [`LogLevel::Debug`] -- available for detail that would otherwise
/// clutter normal operation; nothing currently shipped needs it.
#[allow(dead_code)]
pub fn log_debug(message: &str) {
    log_at(LogLevel::Debug, message);
}

/// A scanned/patched module: its base address plus everything worth logging
/// about the file backing it.
pub struct ModuleInfo {
    pub address: HMODULE,
    pub name: String,
    pub path: String,
    /// On-disk file size in bytes. A game patch changes this almost every
    /// time, even a small one. Together with `modified`, it distinguishes
    /// "the game updated since these signatures were written" from an
    /// unrelated regression in a bug report.
    pub size: u64,
    /// The file's last-modified time, formatted like every log line (see
    /// `format_systemtime`). Converted from the filesystem's UTC `FILETIME`
    /// through `FileTimeToLocalFileTime` first, so it reads on the same
    /// clock as the rest of the log instead of needing a manual timezone
    /// conversion. `"unknown"` if the conversion fails for any reason (e.g.
    /// the file disappears between load and this call).
    pub modified: String,
}

/// Converts a `std::time::SystemTime` to a Win32 `FILETIME` (100ns ticks
/// since 1601-01-01) -- the input format `FileTimeToSystemTime` and
/// `FileTimeToLocalFileTime` expect. `11_644_473_600` is the fixed number of
/// seconds between the Windows epoch and the Unix epoch that every such
/// conversion uses.
fn to_filetime(t: SystemTime) -> Option<FILETIME> {
    let dur = t.duration_since(UNIX_EPOCH).ok()?;
    let ticks = (dur.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(dur.subsec_nanos()) / 100;
    Some(FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 })
}

/// Resolves `path`'s last-modified time to this log's local, ISO-shaped
/// format. See `ModuleInfo::modified`.
fn file_modified_string(path: &str) -> String {
    (|| {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        let utc_ft = to_filetime(modified)?;
        let mut local_ft = FILETIME::default();
        unsafe { FileTimeToLocalFileTime(&utc_ft, &mut local_ft).ok()? };
        let mut st = SYSTEMTIME::default();
        unsafe { FileTimeToSystemTime(&local_ft, &mut st).ok()? };
        Some(format_systemtime(&st))
    })()
    .unwrap_or_else(|| "unknown".to_string())
}

impl ModuleInfo {
    /// Builds a `ModuleInfo` from a module handle, resolving its file name,
    /// size, and modified time for logging.
    pub fn new(address: HMODULE) -> Self {
        let mut buf = [0u16; 260];
        let len = unsafe { GetModuleFileNameW(Some(address), &mut buf) } as usize;
        let path = String::from_utf16_lossy(&buf[..len]);
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let modified = file_modified_string(&path);
        Self { address, name, path, size, modified }
    }
}

/// A mid-function hook keyed off a pattern scan.
pub struct SignatureHook {
    pub tag: &'static str,
    pub signature: &'static str,
    /// Byte offset from the pattern match to the hook point.
    pub offset: usize,
}

/// Converts memory bytes into an IDA-style byte string, e.g. `[0x40, 0x63]` -> `"40 63"`.
pub fn bytes_to_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses an IDA-style byte pattern (e.g. `"48 8B 05 ?? ?? ?? ?? 48 85 C0"`) into a
/// sequence of optional bytes, where `None` is a wildcard.
fn parse_pattern(pattern: &str) -> Vec<Option<u8>> {
    pattern
        .split_whitespace()
        .map(|byte| {
            if byte == "??" {
                None
            } else {
                Some(u8::from_str_radix(byte, 16).expect("invalid byte in AOB pattern"))
            }
        })
        .collect()
}

/// Overwrites memory at `address` with `pattern` (an IDA-style byte string,
/// e.g. `"DE AD BE EF"`), temporarily marking the region writable, then
/// restoring its original protection.
pub fn patch(address: usize, pattern: &str) {
    let bytes: Vec<u8> = pattern
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).expect("invalid byte in patch pattern"))
        .collect();

    unsafe {
        let mut old_protect = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(
            address as *const c_void,
            bytes.len(),
            PAGE_EXECUTE_READWRITE,
            &mut old_protect,
        );
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
        let mut discard = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(address as *const c_void, bytes.len(), old_protect, &mut discard);
    }
}

/// Returns the base address and size of `module`'s loaded image.
fn get_module_range(module: HMODULE) -> Option<(*const u8, usize)> {
    unsafe {
        let process = GetCurrentProcess();
        let mut mod_info = MODULEINFO::default();
        GetModuleInformation(
            process,
            module,
            &mut mod_info,
            std::mem::size_of::<MODULEINFO>() as u32,
        )
        .ok()?;
        Some((mod_info.lpBaseOfDll as *const u8, mod_info.SizeOfImage as usize))
    }
}

/// Slides a window over `data` looking for the first match against `pattern`
/// (`None` entries are wildcards), returning the offset of the match.
fn aob_scan(data: &[u8], pattern: &[Option<u8>]) -> Option<usize> {
    if pattern.is_empty() || data.len() < pattern.len() {
        return None;
    }
    data.windows(pattern.len()).position(|window| {
        window
            .iter()
            .zip(pattern.iter())
            .all(|(byte, pat)| pat.is_none_or(|p| *byte == p))
    })
}

/// Scans `module`'s memory for `signature`, returning the absolute address of the
/// first match, if any.
pub fn pattern_scan(module: HMODULE, signature: &str) -> Option<usize> {
    let (base, size) = get_module_range(module)?;
    let data = unsafe { std::slice::from_raw_parts(base, size) };
    let pattern = parse_pattern(signature);
    aob_scan(data, &pattern).map(|offset| base as usize + offset)
}

/// Scans `module` for every occurrence of `signature`, returning all matching
/// absolute addresses.
///
/// [`pattern_scan`] stops at the first hit, which is right for hook sites but
/// wrong for data like UI strings, where the same text can appear more than
/// once and every copy needs patching.
pub fn pattern_scan_all(module: HMODULE, signature: &str) -> Vec<usize> {
    let Some((base, size)) = get_module_range(module) else {
        return Vec::new();
    };
    let data = unsafe { std::slice::from_raw_parts(base, size) };
    let pattern = parse_pattern(signature);
    let mut hits = Vec::new();
    if pattern.is_empty() {
        return hits;
    }
    let mut i = 0;
    while i + pattern.len() <= data.len() {
        let matched = data[i..i + pattern.len()]
            .iter()
            .zip(pattern.iter())
            .all(|(byte, pat)| pat.is_none_or(|p| *byte == p));
        if matched {
            hits.push(base as usize + i);
            i += pattern.len();
        } else {
            i += 1;
        }
    }
    hits
}

/// Installs a mid-function hook at a fixed `offset` from `module`'s base,
/// bypassing signature scanning. A no-op if `enable` is false.
///
/// Signatures are right for a shipped fix: they survive a game patch moving
/// code, and a wrong match fails loudly rather than silently. They're the
/// wrong tool for instrumenting an address Cheat Engine just found: the
/// offset is already exact, and crafting a signature for each probe wastes
/// the round trip.
///
/// The returned handle is dropped rather than kept: `grapnel::MidHook` has no
/// `Drop`, so a discarded handle just stays installed -- correct here, since
/// this DLL is never unloaded while the game runs. The hook only needs to
/// outlive the process, not be reversible.
///
/// No shipped fix should call this -- every fixed offset a shipped fix once
/// used has since been converted to a signature (see `fixes::visibility` and
/// `fixes::hud::fix_hud_marker_position` for what that conversion looks like).
/// Kept, and deliberately not removed as dead code, for the next investigation
/// that needs to probe an address before it is worth a signature at all.
#[allow(dead_code)]
pub fn inject_hook_at<F>(
    enable: bool,
    module: &ModuleInfo,
    tag: &str,
    offset: usize,
    callback: F,
) -> bool
where
    F: FnMut(&mut Context) + Send + 'static,
{
    if !enable {
        return false;
    }
    let hook_addr = (module.address.0 as usize + offset) as *mut u8;
    match MidHook::install(hook_addr, callback) {
        Ok(_hook) => {
            log(&format!("{tag} : Hooked @ {}+{offset:x}", module.name));
            true
        }
        Err(e) => {
            log_error(&format!("{tag} : Failed to install hook @ {}+{offset:x}: {e}", module.name));
            false
        }
    }
}

/// Scans `module` for `sh.signature` and, if found, installs a mid-function hook
/// at the match plus `sh.offset`, calling `callback` with the full register
/// context on every hit. A no-op if `enable` is false; `true` means it was
/// installed. See [`inject_hook_at`] for why the handle is not kept.
#[allow(dead_code)]
pub fn inject_hook<F>(enable: bool, module: &ModuleInfo, sh: &SignatureHook, callback: F) -> bool
where
    F: FnMut(&mut Context) + Send + 'static,
{
    if !enable {
        return false;
    }
    match pattern_scan(module.address, sh.signature) {
        Some(addr) => {
            let rel_addr = addr - module.address.0 as usize;
            log(&format!("{} : Found '{}' @ {}+{:x}", sh.tag, sh.signature, module.name, rel_addr));
            let hook_addr = (addr + sh.offset) as *mut u8;
            match MidHook::install(hook_addr, callback) {
                Ok(_hook) => {
                    log(&format!("{} : Hooked @ {}+{:x}", sh.tag, module.name, rel_addr + sh.offset));
                    true
                }
                Err(e) => {
                    log_error(&format!("{} : Failed to install hook for '{}': {e}", sh.tag, sh.signature));
                    false
                }
            }
        }
        None => {
            log_error(&format!("{} : Did not find '{}'", sh.tag, sh.signature));
            false
        }
    }
}
