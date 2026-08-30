mod config;
mod fixes;
mod utils;

use std::ffi::c_void;

use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU32, Ordering};

use utils::{ModuleInfo, init_log_file, log, log_error};
use windows::Win32::Foundation::{CloseHandle, HMODULE};
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::System::Threading::{
    CreateThread, SetThreadPriority, THREAD_CREATION_FLAGS, THREAD_PRIORITY_HIGHEST,
};

/// The aspect the game itself stores, and what every correction is measured
/// against.
const GAME_ASPECT: f32 = 16.0 / 9.0;

/// The render width and height every fix measures against, resolved once at
/// startup by [`config::resolve_resolution`] eitehr from the config file if
/// set, otherwise the primary display's current resolution.
static RENDER_WIDTH: AtomicU32 = AtomicU32::new(0);
static RENDER_HEIGHT: AtomicU32 = AtomicU32::new(0);

fn log_open(module: &ModuleInfo) {
    init_log_file("samurai_warriors5_fix.log");

    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    log("-------------------------------------");
    log(&format!("Version: {}-{} ({profile})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")));
    log(&format!("Rustc: {}", env!("RUSTC_VERSION")));
    log(&format!("Module Name: {}", module.name));
    log(&format!("Module Path: {}", module.path));
    log(&format!("Module Addr: {:#x}", module.address.0 as usize));
    log(&format!("Module Size: {:#x} ({} bytes)", module.size, module.size));
    log("-------------------------------------");
}

/// True entry point of the DLL.
///
/// Applies all fixes and features of this mod to the game process it was
/// attached to based on the `Config` using `this_module` to determine
/// where the path on the filesystem where the exe is stored.
///
/// Panics are isolated and captured so if one is thrown it does not crash
/// the process and it is logged for debugging.
fn main(this_module: HMODULE) {
    // Catch a panic's message and location reach the log file rather than vanishing.
    std::panic::set_hook(Box::new(|info| log_error(&format!("panic: {info}"))));

    // Catches a panic here instead of letting it crash the process.
    let result = catch_unwind(|| {
        let handle = unsafe { GetModuleHandleA(None) }.expect("failed to get module handle");
        let module = ModuleInfo::new(handle);

        log_open(&module);

        let config = config::load(this_module);
        if !config.super_enable {
            log("main: super_enable is false in the config, mod not applied");
            return;
        }

        if let Some((w, h)) = config::resolve_resolution(&config) {
            RENDER_WIDTH.store(w, Ordering::Relaxed);
            RENDER_HEIGHT.store(h, Ordering::Relaxed);
        }

        fixes::resolution::fix_resolution_list(&module);
        fixes::hud::fix_hud(&module);
        fixes::visibility::fix_npc_visibility(&module);
    });

    if result.is_err() {
        log_error("main: panicked -- see the panic line above in this log for details");
    }
}

/// Windows ABI compatible entry point.
///
/// Becuase this is provided to the `CreateThread` Windows API function this
/// function needs to compiled to use the appropriate Windows ABI. Afterwards
/// `main`` can be called safely using the Rust ABI.
unsafe extern "system" fn windows_main(param: *mut c_void) -> u32 {
    main(HMODULE(param));
    0
}

/// Entry point of the DLL.
#[unsafe(no_mangle)]
extern "system" fn DllMain(hinst: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            if let Ok(thread) = CreateThread(
                None,
                0,
                Some(windows_main),
                Some(hinst.0 as *const c_void),
                THREAD_CREATION_FLAGS(0),
                None,
            ) {
                let _ = SetThreadPriority(thread, THREAD_PRIORITY_HIGHEST);
                let _ = CloseHandle(thread);
            }
        }
    }
    1
}
