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
/// startup by [`config::resolve_resolution`] -- from the config file if set,
/// otherwise the primary display's current mode.
static RENDER_WIDTH: AtomicU32 = AtomicU32::new(0);
static RENDER_HEIGHT: AtomicU32 = AtomicU32::new(0);

fn log_open(module: &ModuleInfo) {
    init_log_file("samurai_warriors5_fix.log");

    // GIT_HASH/RUSTC_VERSION come from build.rs, which shells out to `git`
    // and `rustc` at compile time -- there's no other way to get either into
    // a compiled binary, since Cargo doesn't expose them as env vars itself.
    // Pinpointing the exact source commit matters more here than
    // CARGO_PKG_VERSION: that only changes when someone remembers to bump
    // Cargo.toml, while every real change already has a commit.
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    log("-------------------------------------");
    log(&format!("Version: {}-{} ({profile})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")));
    log(&format!("Rustc: {}", env!("RUSTC_VERSION")));
    log(&format!("Module Name: {}", module.name));
    log(&format!("Module Path: {}", module.path));
    log(&format!("Module Addr: {:#x}", module.address.0 as usize));
    // Size/modified identify exactly which build of the game this is --
    // signatures in fixes/ are matched against specific game bytes, and a
    // patch changing them is the main way a signature scan can start failing.
    log(&format!("Module Size: {:#x} ({} bytes)", module.size, module.size));
    log(&format!("Module Modified: {}", module.modified));
    log("-------------------------------------");
}

/// Applies every fix. Runs once, on a thread spawned from `DllMain`, and never
/// exits or unwinds it: every hook here is meant to outlive the process, not
/// be reversible. `MidHook::uninstall` cannot prove no thread is still
/// executing inside a trampoline without first suspending every thread, so
/// removing a hook while the game runs risks a crash if another thread is
/// mid-call through it. One-shot, permanent hooks sidestep the problem
/// entirely: nothing is ever removed while the process is alive, so there is
/// nothing to race.
/// `this_module` is *this DLL's own* handle -- `DllMain`'s first parameter,
/// threaded through via `CreateThread`'s `lpParameter` -- not the game's. It
/// only exists to locate the config file next to this DLL regardless of the
/// game's current working directory; nothing else here needs it.
fn init(this_module: HMODULE) {
    // There is no console to catch the default panic hook's output -- a
    // full-screen game with no console attached leaves it with nowhere to go
    // -- so this is the only way a panic's message and location reach the log
    // file rather than vanishing.
    std::panic::set_hook(Box::new(|info| log_error(&format!("panic: {info}"))));

    // Gracefully isolates a panic if one where to happen so it does not crash
    // the process.
    let result = catch_unwind(|| {
        let handle = unsafe { GetModuleHandleA(None) }.expect("failed to get module handle");
        let module = ModuleInfo::new(handle);

        log_open(&module);

        let config = config::load(this_module);
        if !config.super_enable {
            log("core: super_enable is false in the config, mod not applied");
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
        log_error("core: init panicked -- see the panic line above in this log for details");
    }
}

unsafe extern "system" fn init_thread(param: *mut c_void) -> u32 {
    init(HMODULE(param));
    0
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(hinst: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            if let Ok(thread) = CreateThread(
                None,
                0,
                Some(init_thread),
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
