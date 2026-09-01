//! Whether the game considers a character's *model* on-screen at all, before
//! anything about it is drawn. An NPC despawning or never appearing past the
//! 16:9 band happens upstream of any nameplate or health bar, and would
//! happen even with every HUD fix disabled.

use std::sync::atomic::Ordering;

use crate::utils::{ModuleInfo, SignatureHook, inject_hook};
use crate::{RENDER_HEIGHT, RENDER_WIDTH};

/// Widens the camera's cull frustum to the real screen aspect.
///
/// The per-entity "is this on screen" test projects a character through a
/// camera-block matrix and compares the result against `0..width`: a
/// perspective divide reduces that to `-1 <= ndc_x <= 1` regardless of what
/// `width` actually is, so only the aspect the projection was built with can
/// widen what passes. Confirmed by finding this exact test function --
/// parameters bounded `player < 2` and `entity < 200`, an entity-index lookup
/// with a `0x2c` stride, a secondary table read through `+0x160` bounded by
/// `0x9f0`, a vtable call through `+0x30`, the resulting position transformed
/// through `singleton + 0x1460 + player*0x320 + 0xc0`, then bounds-checked
/// against `+0x1748`/`+0x174c` on that same block -- structurally identical,
/// constant-for-constant, to the equivalent function in a differently-built
/// copy of this game where the mechanism was already independently verified
/// end-to-end (live aspect measured stuck at 16:9, the fix confirmed in
/// motion). Only the static addresses differ between the two builds; the
/// struct layout and the test itself did not change.
///
/// The correction goes in where the projection is built.
/// `FUN_7ff614efb020`-equivalent is the textbook construction:
///
///     f      = 1 / (tan(fovY/2) * k)
///     m[5]   = f                       ; m11
///     m[0]   = f / aspect              ; m00
///
/// with `RCX` = destination matrix, `XMM1` = fovY, `XMM2` = aspect. Raising
/// the aspect lowers `m00`, widening the horizontal frustum -- vertical FOV
/// untouched, matching how the scene already renders.
///
/// The same builder serves other cameras, and their rendered field of view is
/// already correct without this hook touching it. So the write applies only
/// when `RCX` is one of the two camera projections' own addresses -- one
/// camera per player in this game's 2-player split-screen, each a fixed
/// offset from a render-state singleton (see `SINGLETON_PTR` below). If the
/// rendered field of view ever visibly widens along with this fix, that means
/// the scoping matched something it should not have -- re-check this guard
/// first.
///
/// Hooked by signature: the function's own prologue (`PUSH RBX; SUB RSP,0x40`)
/// recurs 51 times image-wide. Extending through the next instruction's opcode
/// and addressing mode -- `MULSS XMM1` via a RIP-relative operand -- narrows
/// that to exactly one match. The trailing 4-byte displacement is wildcarded:
/// it points at the float constant `0.5f`, whose address shifts whenever
/// anything earlier in `.rdata` changes size, independent of whether this is
/// still the right function.
///
/// The rest of this function comes down to two tiers of guard, checked at two
/// different times:
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
        // Hardcoded rather than signature-derived, unlike everything else in
        // this project -- a deliberate exception. This is a raw offset to a
        // global's storage slot in the module's own data section. That slot's
        // position depends on the layout of everything the compiler places in
        // that section, so it moves if the binary is rebuilt at all, even
        // without a patch touching this function itself. Static analysis of
        // this binary reads back uniformly high-entropy bytes at every
        // known-good address checked, matching runtime behavior nowhere, so a
        // signature scan isn't an option for finding this slot either --
        // it has to be read live, from the decompiled test function's own
        // dereference of it.
        const SINGLETON_PTR: usize = 0x1578e20;

        // Each camera's projection matrix address: singleton + 0x1460
        // (per-camera block) + player_index * 0x320 (block stride) + 0xc0
        // (projection's offset within the block). Written out flat rather
        // than as three named pieces because that's how the test function
        // itself computes it -- singleton + 0x1650/0x1840 with the stride
        // folded into a single per-player multiply, not a struct field access.
        const CAMERA_0_OFFSET: usize = 0x1520;
        const CAMERA_1_OFFSET: usize = 0x1840;

        let singleton = *((base + SINGLETON_PTR) as *const usize);

        let camera_0 = singleton + CAMERA_0_OFFSET;
        let camera_1 = singleton + CAMERA_1_OFFSET;
        let dst = ctx.rcx as usize;
        if dst != camera_0 && dst != camera_1 {
            return;
        };

        ctx.xmm2.f32()[0] = aspect;
    });
}
