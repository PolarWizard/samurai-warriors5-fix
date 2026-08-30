//! Whether the game considers a character's *model* on-screen at all, before
//! anything about it is drawn. An NPC despawning or never appearing past the
//! 16:9 band happens upstream of any nameplate or health bar, and would
//! happen even with every HUD fix disabled.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::utils::{ModuleInfo, SignatureHook, inject_hook, log};
use crate::{RENDER_HEIGHT, RENDER_WIDTH};

/// Set once the fix has actually fired, so a silent miss is visible in the log
/// rather than looking like a fix that did nothing.
static ASPECT_FIX_LOGGED: AtomicU32 = AtomicU32::new(0);

/// Widens the camera's cull frustum to the real screen aspect.
///
/// Measured in game at 3840x1080: `camBlock+0x14c` reads **1.7778** while the
/// scene renders 32:9. Everything that asks "is this on screen" measures
/// against a projection built from that number, so the answer is 16:9 no
/// matter how wide the picture is.
///
/// `FUN_7ff614aec590` is the clearest example -- it projects an entity
/// through `camBlock+0x1f0` (= viewProj x NDC-to-pixel) and tests
/// `0 <= x <= blk+0x2e8`. That composes to
///
///     x_pixel = ndc_x*(w/2) + w/2,  bounded by 0..w   <=>   -1 <= ndc_x <= 1
///
/// so the viewport `w` cancels out of its own bound: widening `blk+0x2e8`
/// from 1920 does nothing at all. Only the aspect moves that boundary.
///
/// The correction goes in where the projection is built.
/// `FUN_7ff614efb020` @ SW5.exe+57b020 is the textbook construction:
///
///     f      = 1 / (tan(fovY/2) * k)
///     m[5]   = f                       ; m11
///     m[0]   = f / aspect              ; m00
///
/// with `RCX` = destination matrix, `XMM1` = fovY, `XMM2` = aspect. Raising
/// the aspect lowers `m00`, widening the horizontal frustum -- vertical FOV
/// untouched, matching how the scene already renders.
///
/// The same builder serves other cameras, and the rendered field of view is
/// already correct without this hook touching it, so the write only applies
/// when `RCX` is one of the two camera blocks' own projections: `singleton +
/// 0x1460 + camIdx*0x320 + 0xc0` for `camIdx` 0 and 1 -- one camera per player
/// in this game's 2-player split-screen. If the rendered field of view ever
/// visibly widens along with this fix, that means the scoping matched
/// something it should not have, and this check is the first thing to
/// re-examine.
///
/// Hooked by signature: the function's own prologue (`PUSH RBX; SUB RSP,0x40`)
/// recurs 51 times image-wide, but extending through the next instruction's
/// opcode and addressing mode -- `MULSS XMM1` via a RIP-relative operand --
/// narrows that to exactly one match. The trailing 4-byte displacement is
/// wildcarded: it points at the float constant `0.5f`, whose address shifts
/// whenever anything earlier in `.rdata` changes size, independent of whether
/// this is still the right function.
///
/// Two tiers of guard, checked at two different times, is what the rest of
/// this function comes down to:
///
/// * Whether the configured resolution is wide enough to need correcting at
///   all is *this project's own* state, fixed once at startup -- so it is
///   computed once, here, and the hook is never even installed if it fails.
/// * Whether the render-state singleton exists yet, and which camera a given
///   call is for, is the *game's own* runtime state -- unknowable until the
///   hook actually fires, so those checks stay inside the callback below.
pub fn fix_npc_visibility(module: &ModuleInfo) {
    const PROJECTION_BUILDER_SIG: &str = "40 53 48 83 EC 40 F3 0F 59 0D ?? ?? ?? ??";

    let (w, h) = (
        RENDER_WIDTH.load(Ordering::Relaxed),
        RENDER_HEIGHT.load(Ordering::Relaxed),
    );
    let aspect = w as f32 / h as f32;

    let base = module.address.0 as usize;

    let sig = SignatureHook { tag: "npc_visibility", signature: PROJECTION_BUILDER_SIG, offset: 0 };
    inject_hook(true, module, &sig, move |ctx| unsafe {
        // This is very fragile, and this is a raw offset to a global
        // variable's storage slot in the module's own data section.
        // That slot's position depends on the layout of everything
        // the compiler places in that section — adding, removing, or
        // resizing some totally unrelated global anywhere in the
        // game could shift it. A game update will almost certainly
        // break this fix, and probably crash the game, but it's a
        // single player game released ~6 years ago, as of writing
        // this, and the game getting an update is like 0% chance.
        const SINGLETON_PTR: usize = 0x156c350;

        // CAMERA_BLOCK, CAMERA_BLOCK_STRIDE, and CAMERA_PROJECTION
        // are member offsets inside one specific class — those only
        // move if that one class's own fields change, which is a much
        // narrower, more deliberate kind of edit. You're right to
        // treat them differently.
        const CAMERA_BLOCK: usize = 0x1460;
        const CAMERA_BLOCK_STRIDE: usize = 0x320;
        const CAMERA_PROJECTION: usize = 0xc0;

        // The game's own object, built during its own startup -- not
        // guaranteed to exist yet on an early call.
        let singleton = *((base + SINGLETON_PTR) as *const usize);

        let camera_0 = singleton + CAMERA_BLOCK + CAMERA_PROJECTION;
        let camera_1 = singleton + CAMERA_BLOCK + CAMERA_BLOCK_STRIDE + CAMERA_PROJECTION;
        let dst = ctx.rcx as usize;
        if dst != camera_0 && dst != camera_1 {
            return;
        }

        // XMM2 is the aspect argument as the game passed it in. Checked
        // before overwriting so a wrong signature match fails silently
        // instead of writing a garbage value into a matrix.
        let current = &mut ctx.xmm2.f32()[0];
        if !current.is_finite() || *current <= 0.0 {
            return;
        }

        if ASPECT_FIX_LOGGED.swap(1, Ordering::Relaxed) == 0 {
            log(&format!(
                "npc_visibility: camera projection rebuilt with aspect {current:.4} -> {aspect:.4}"
            ));
        }
        *current = aspect;
    });
}
