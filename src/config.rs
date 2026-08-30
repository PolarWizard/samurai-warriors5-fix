//! User-facing settings, loaded once from a TOML file next to this DLL.
//!
//! Mirrors the sibling SamuraiWarriors4DXFix project's `[resolution]` section:
//! width/height of 0 (or the file missing entirely) means auto-detect from
//! the primary display's current mode, same as that project's
//! `getDesktopDimensions` fallback.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::core::PCWSTR;

use crate::utils::{log, log_error, log_warn};

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    pub super_enable: bool,
    pub resolution: ResolutionConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self { super_enable: true, resolution: ResolutionConfig::default() }
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ResolutionConfig {
    pub width: u32,
    pub height: u32,
}

/// Reads `samurai_warriors5_fix.toml` from the same directory as `this_module`,
/// then logs whichever config ends up in effect, regardless of which path
/// below produced it. That includes the disabled-mod case: otherwise,
/// disabling the mod would be the one path that never shows what the file
/// actually contained.
pub fn load(this_module: HMODULE) -> Config {
    let config = load_inner(this_module);
    log(&format!("config: {config:?}"));
    config
}

/// `this_module` must be *this DLL's own* handle (`DllMain`'s first
/// parameter), not the game's. A relative filename would depend on the
/// process's current working directory -- for a launched game, that's its
/// install root, not the `scripts` folder this DLL and its config actually
/// live in.
///
/// Falls back to defaults (auto-detect everything) on any error -- a missing
/// or malformed config should degrade gracefully, not stop every fix in the
/// mod from applying.
fn load_inner(this_module: HMODULE) -> Config {
    let Some(dir) = module_dir(this_module) else {
        log_warn("config: could not resolve this DLL's own directory, using defaults");
        return Config::default();
    };
    let path = dir.join("samurai_warriors5_fix.toml");

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            log(&format!("config: {} not found ({e}), using defaults", path.display()));
            return Config::default();
        }
    };

    match toml::from_str(&text) {
        Ok(config) => {
            log(&format!("config: loaded {}", path.display()));
            config
        }
        Err(e) => {
            log_warn(&format!("config: {} failed to parse ({e}), using defaults", path.display()));
            Config::default()
        }
    }
}

fn module_dir(module: HMODULE) -> Option<PathBuf> {
    let mut buf = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    if len == 0 {
        return None;
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
        .parent()
        .map(|p| p.to_path_buf())
}

/// Resolves the render resolution to use: the config's override if both
/// dimensions are set, otherwise the primary display's current mode.
///
/// Resolved once, at startup, into the atomics every fix reads -- not
/// watched afterward. Changing resolution or resizing mid-session has no
/// effect until the process restarts (or the config is edited and the
/// process restarted), same as the sibling SamuraiWarriors4DXFix project.
pub fn resolve_resolution(config: &Config) -> Option<(u32, u32)> {
    if config.resolution.width != 0 && config.resolution.height != 0 {
        log(&format!(
            "config: using configured resolution {}x{}",
            config.resolution.width, config.resolution.height
        ));
        return Some((config.resolution.width, config.resolution.height));
    }

    let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    let ok = unsafe { EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool();
    if !ok || mode.dmPelsWidth == 0 || mode.dmPelsHeight == 0 {
        log_error("config: could not read the primary display's current mode, and no resolution is configured");
        return None;
    }
    log(&format!(
        "config: auto-detected resolution {}x{} from the primary display",
        mode.dmPelsWidth, mode.dmPelsHeight
    ));
    Some((mode.dmPelsWidth, mode.dmPelsHeight))
}
