//! Everything drawn as HUD: the 2D UI (menus, text, minimap) and the
//! world-space HUD (nameplates, health bars, and other markers anchored to a
//! 3D position but drawn as flat screen-space quads).
//!
//! Unrelated to [`crate::fixes::visibility`]: whether a character's *model* is
//! considered on-screen is a separate system from anything drawn here. Every
//! fix in this file assumes that question is already settled.
//!
//! Every fix below follows the same two-tier guard split as
//! [`crate::fixes::visibility`]: whatever depends only on the resolved
//! resolution -- fixed once at startup -- is computed once, before the hook
//! installs, and captured by value into the closure. Whatever depends on the
//! specific call the game is making right now (a register that could
//! legitimately be null or out of range on any given call) is checked inside
//! the closure, because it can only be known there.

use std::sync::atomic::Ordering;

use crate::utils::{ModuleInfo, SignatureHook, inject_hook, log, log_error, pattern_scan};
use crate::{GAME_ASPECT, RENDER_HEIGHT, RENDER_WIDTH};

/// Applies every HUD fix, 2D and world-space.
pub fn fix_hud(module: &ModuleInfo) {
    fix_hud_projection_shape(module);
    fix_hud_text_scale(module);
    fix_minimap_scale(module);
    fix_minimap_position_gameplay(module);
    fix_minimap_position_menu(module);
    fix_hud_marker_position(module);
    fix_hud_marker_visibility(module);
}

/// Un-stretches the 2D UI and the world-space HUD by correcting the
/// horizontal scale of whichever projection matrix a HUD draw call uses.
///
/// The vertex shader's constant buffer holds the UI projection:
///
///   +040   1.3580   0.0000   0.0000   0.0000    <- m00, horizontal scale
///   +050   0.0000   2.4142   0.0000   0.0000    <- m11, vertical scale
///
/// 2.4142 / 1.3580 = 1.7778 -- exactly 16/9. The projection is built with a
/// *hardcoded* 16:9 aspect while rendering to a 32:9 target, so everything
/// it draws comes out stretched horizontally by 32:9 / 16:9 = 2x. For
/// 3840x1080 the correct m00 is 2.4142 / 3.5556 = 0.679.
///
/// The per-element positions live in world matrices further along that same
/// constant buffer and are correct as authored -- only the projection they
/// are drawn through is wrong, which is why one number fixes the entire UI.
///
/// The same pair is mirrored on the draw context this hook already
/// receives in RCX: +0x0e0 is m00 and +0x0f4 is m11 (a second copy sits at
/// +0x120 / +0x134). Rewriting m00 from m11 and the *real* aspect fixes the
/// entire UI in one place, before any of it is drawn.
///
/// Deriving m00 from m11 rather than scaling the old value keeps this
/// correct at any resolution and idempotent when it runs every frame.
fn fix_hud_projection_shape(module: &ModuleInfo) {
    const HUD_LAYOUT_SIG: &str = "48 89 5C 24 18    48 89 74 24 20    55    41 54    41 55    41 56    41 57    48 8B EC    48 83 EC 60    48 8B 05 ?? ?? ?? ??";
    const PROJ_M00: usize = 0x0e0;
    const PROJ_M11: usize = 0x0f4;
    const PROJ_M00_ALT: usize = 0x120;
    const PROJ_M11_ALT: usize = 0x134;

    let (w, h) = (
        RENDER_WIDTH.load(Ordering::Relaxed),
        RENDER_HEIGHT.load(Ordering::Relaxed),
    );
    let aspect = w as f32 / h as f32;

    let sig = SignatureHook { tag: "hud_projection_shape", signature: HUD_LAYOUT_SIG, offset: 0 };
    inject_hook(true, module, &sig, move |ctx| {
        // RCX is the draw context the game passed in for this call
        if ctx.rcx == 0 {
            return;
        }

        unsafe {
            let base = ctx.rcx as usize;

            for (m00_off, m11_off) in [(PROJ_M00, PROJ_M11), (PROJ_M00_ALT, PROJ_M11_ALT)] {
                let m00 = (base + m00_off) as *mut f32;
                let m11 = *((base + m11_off) as *const f32);
                if !m11.is_finite() || m11 <= 0.0 {
                    continue;
                }

                // Two matrices show up at these offsets and they need
                // opposite treatment, so the branch below splits them by
                // magnitude: perspective terms are order ~1, orthographic ones
                // are order ~2/resolution. Their ratio cannot separate them --
                // both are 1.7778.
                //
                // The orthographic one belongs to the world-space HUD, not
                // the minimap: the minimap draws through a correct 2/3840
                // constant buffer. Its own buffer reads
                //   +040  0.00104 0 0 0     <- 2/1920
                //   +050  0 0.00185 0 0     <- 2/1080
                // a projection authored for a 1920-wide space while the target
                // is 3840, so one ortho unit covers 2px horizontally and 1px
                // vertically -- the horse icon measures 144x72 where a circle
                // must be 72x72.
                //
                // Correcting m00 here fixes shape but not position: it halves
                // pixels-per-ortho-unit, so every element then draws at half
                // its offset from screen centre (`pixel_x = ortho_x * (W*m00/2)
                // + W/2` scales both). `fix_hud_marker_position` below is the
                // other half -- it widens the world-to-screen projection that
                // produces each element's ortho-space position in the first
                // place, upstream of this matrix entirely.
                if (0.0005..=0.01).contains(&m11) {
                    // Only +0x120. Tested each way with name plates on screen:
                    // +0xe0 alone leaves them stretched, +0x120 alone fixes
                    // them, so correcting +0xe0 was pure collateral on whatever
                    // else that matrix serves.
                    *m00 = if m00_off == PROJ_M00_ALT {
                        m11 / aspect
                    } else {
                        m11 / GAME_ASPECT
                    };
                    continue;
                }

                if !(0.1..=100.0).contains(&m11) {
                    continue;
                }
                *m00 = m11 / aspect;
            }
        }
    });
}

/// Keeps on-screen text at its correct size instead of scaling up to fill the
/// wider width.
///
/// Hooked right before `call FUN_7ff6150e2ee0`, which feeds into:
///   *(float *)(param_1 + 0x234) = (float)EBX / (float)*(int *)(rax + 0x300)
/// EBX is the width numerator; the denominator comes off a render-target/
/// viewport struct (a height). The result is a per-object scale factor
/// the text renderer reads afterward -- structurally the same pattern as
/// the camera-aspect fix in [`crate::fixes::visibility`], just feeding text
/// instead of the camera.
///
/// At 3840x1080, EBX is naturally the real screen width (3840), so the
/// game divides as if the monitor were 3840x2160: text scales up to match
/// the width, then overflows vertically off-screen. Forcing EBX to the
/// 16:9-equivalent width for the resolved height makes the game compute the
/// ratio it would at native 16:9, so text renders at the correct size,
/// centered on screen -- same derivation as [`fix_minimap_scale`] below,
/// since both feed the same kind of width/height ratio.
fn fix_hud_text_scale(module: &ModuleInfo) {
    let h = RENDER_HEIGHT.load(Ordering::Relaxed);
    let reference_width = (h as f32 * 16.0 / 9.0).round() as u64;

    const TEXT_SCALE_SIG: &str = "E8 ?? ?? ?? ??    33 D2    48 8B 88 ?? ?? ?? ??    48 81 C1 ?? ?? ?? ??";
    let sig = SignatureHook { tag: "hud_text_scale", signature: TEXT_SCALE_SIG, offset: 0 };
    inject_hook(true, module, &sig, move |ctx| {
        ctx.rbx = reference_width;
    });
}

/// The same text-scale calculation again, the minimap's own copy of it.
///
/// SW5.exe+12C4C0 is a sibling of the text-scale function -- structurally
/// identical, reading the render width and dividing it by a reference:
///
///   +12C4F6   mov ebx,[rax+00003048]    <- render width (3840)
///   +12C4FC   call SW5.exe+D789B0
///   +12C501   mov ecx,[rax+00000300]    <- denominator
///   +12C51E   divss xmm1,xmm0           <- returns width / denominator
///
/// This is why `fix_hud_projection_shape` never reaches the minimap: it
/// doesn't go through the shared UI projection at all -- it derives its own
/// scale here. Forcing EBX the same way `fix_hud_text_scale` does gives the
/// minimap its correct size.
///
/// Hooked at the `call` rather than the `mov`, so the load has already
/// happened and our value is what survives into the divide. RBX is
/// non-volatile under the MS x64 ABI, so the call preserves it.
///
/// Derived from the resolved height rather than hardcoded, so it stays
/// correct at other resolutions: 1080 -> 1920, 1440 -> 2560, 2160 -> 3840
/// (same derivation [`fix_hud_text_scale`] above uses). The height cannot
/// change after startup, so the ratio is computed once below and every call
/// just writes the same constant into RBX.
fn fix_minimap_scale(module: &ModuleInfo) {
    const MINIMAP_SCALE_SIG: &str = "8B 98 48 30 00 00    E8 ?? ?? ?? ??    8B 88 00 03 00 00";

    let h = RENDER_HEIGHT.load(Ordering::Relaxed);
    let reference_width = (h as f32 * 16.0 / 9.0).round() as u64;

    let sig = SignatureHook { tag: "minimap_scale", signature: MINIMAP_SCALE_SIG, offset: 6 };
    inject_hook(true, module, &sig, move |ctx| {
        ctx.rbx = reference_width;
    });
}

/// Re-centres the in-battle minimap into the 16:9 band, in the caller that
/// actually draws it.
///
/// The runtime caller probe named it: the in-game minimap comes from
/// FUN_7ff614c5ab10 (return address SW5.exe+2dac4e), and the menu map (see
/// [`fix_minimap_position_menu`]) from FUN_7ff614bff100 (+27f13e).
///
/// Its rect is built with SIMD rather than the scalar stores the other
/// caller used -- eight floats, two `movups`, with RDX pointing at them:
///
///   +ac67  lea    rdx,[rbp+0x27]      <- RDX = &rect (8 floats)
///   +ac6f  mulps  xmm1,xmm3           <- 4 floats * scale
///   +ac72  mulps  xmm2,xmm3           <- 4 floats * scale
///   +ac75  movups [rbp+0x37],xmm1
///   +ac79  movups [rbp+0x27],xmm2
///   +ac7d  mov    rax,[rcx]           <- hooked here, both halves written
///   +ac80  call   qword ptr [rax+0x18]
///
/// Eight floats are most likely four (x, y) corners, so the X components --
/// the even indices -- are what's shifted, by a fixed offset that depends
/// only on the resolved width and height.
fn fix_minimap_position_gameplay(module: &ModuleInfo) {
    const MINIMAP_GAMEPLAY_RECT_SIG: &str = "0F 11 4D 37    0F 11 55 27    48 8B 01    FF 50 18";

    let (w, h) = (
        RENDER_WIDTH.load(Ordering::Relaxed),
        RENDER_HEIGHT.load(Ordering::Relaxed),
    );
    let band_offset = (w as f32 - h as f32 * 16.0 / 9.0) * 0.5;

    let sig = SignatureHook {
        tag: "minimap_position_gameplay",
        signature: MINIMAP_GAMEPLAY_RECT_SIG,
        offset: 8,
    };
    inject_hook(true, module, &sig, move |ctx| {
        if ctx.rdx == 0 {
            return;
        }
        unsafe {
            let rect = ctx.rdx as *mut f32;
            *rect.add(0) += band_offset;
            *rect.add(2) += band_offset;
            *rect.add(4) += band_offset;
            *rect.add(6) += band_offset;
        }
    });
}

/// Re-centres the "Prepare for Battle" / Info Screen map into the 16:9 band.
///
/// Drawn by FUN_7ff614bff100, the other live caller the runtime probe
/// named (return address SW5.exe+27f13e). Structurally the simplest of the
/// three: a straight elementwise scale of eight floats, then one draw.
///
///   local_48 = scale * param_2[0];  ... local_2c = scale * param_2[7];
///   (**(code **)**(...+0xd8))(obj, &local_48, param_3, param_4);
///
///   +f145  lea    rdx,[rsp+0x30]     <- RDX = &rect (8 floats)
///   ...    eight movss stores to [rsp+0x30 .. +0x4c]
///   +f1bf  mov    rax,[rcx]          <- hooked here, all eight written
///   +f1c9  call   qword ptr [rax]
///
/// Same shift as [`fix_minimap_position_gameplay`] on the even (X) indices --
/// same formula too, `(w - h*16/9) * 0.5`, the width of the margin outside
/// the centred 16:9 band on one side.
fn fix_minimap_position_menu(module: &ModuleInfo) {
    const MENU_MAP_RECT_SIG: &str = "F3 0F 11 5C 24 3C    48 8B 01    F3 0F 11 44 24 20    FF 10";

    let (w, h) = (
        RENDER_WIDTH.load(Ordering::Relaxed),
        RENDER_HEIGHT.load(Ordering::Relaxed),
    );
    let band_offset = (w as f32 - h as f32 * 16.0 / 9.0) * 0.5;

    let sig = SignatureHook { tag: "minimap_position_menu", signature: MENU_MAP_RECT_SIG, offset: 6 };
    inject_hook(true, module, &sig, move |ctx| {
        if ctx.rdx == 0 {
            return;
        }
        unsafe {
            let rect = ctx.rdx as *mut f32;
            *rect.add(0) += band_offset;
            *rect.add(2) += band_offset;
            *rect.add(4) += band_offset;
            *rect.add(6) += band_offset;
        }
    });
}

/// Widens the world-to-screen projection so world-space HUD markers (name
/// plates, health bars, the horse icon) follow their character across the
/// whole screen instead of compressing toward centre.
///
/// `FUN_7ff614c12930` turns a character's world position into a screen pixel:
///
///     int4 vp = *(int4*)([SW5.exe+156C350] + 0x1c90 + cam*0x34);  // {1920,1080,x,y}
///     halfW   = vp.w * 0.5                                        // 960
///     screenX = ndc_x * halfW + (vp.x + halfW)
///
/// The viewport rect is authored 1920x1080 and never rebuilt from the real
/// backbuffer, so `halfW` stays 960 at any resolution and every marker is
/// placed as though the screen were 16:9. `fix_hud_projection_shape` fixes
/// each element's *shape* but halves pixels-per-ortho-unit, so the positions
/// this routine produces then land at half their offset from centre.
///
/// The scale and the centre are separate instructions here, which is what
/// makes this the right place:
///
///     +292ae1  ADDSS  XMM0,XMM1     ; centre  = vp.x + halfW
///     +292ae5  MULSS  XMM8,XMM1     ; offset  = ndc_x * halfW   <-- hooked
///
/// Scaling XMM1 at the second one widens the offset and leaves the centre
/// alone, so a character at screen centre does not move and one at the edge
/// of the 16:9 frustum reaches the edge of the panel. Each character is
/// corrected once, before its plate's parts are laid out around it, so the
/// parts keep their authored offsets -- no grouping of transforms, and
/// nothing that can drag two neighbouring plates to a shared anchor.
///
/// The rect itself is deliberately not patched: SW5.exe+3e5490 and +da370
/// read the same rect to build the GPU viewport, so widening it too would
/// widen the viewport as well.
///
/// Found by signature ending at the hooked instruction, not the containing
/// function's distant entry point: `+292adc..+292ae5` (`MULSS XMM8,XMM4;
/// ADDSS XMM0,XMM1`) is a fully literal 9 bytes -- no addresses or
/// displacements to wildcard -- and confirmed unique image-wide by exhaustive
/// scan. Anchoring this close to the target means a future rebuild only
/// breaks this signature if these two adjacent, data-dependent instructions
/// themselves change, not if anything earlier in the function's ~430 bytes of
/// setup does.
fn fix_hud_marker_position(module: &ModuleInfo) {
    const MARKER_POSITION_SIG: &str = "F3 44 0F 59 C4 F3 0F 58 C1";

    let (w, h) = (
        RENDER_WIDTH.load(Ordering::Relaxed),
        RENDER_HEIGHT.load(Ordering::Relaxed),
    );
    let scale = (w as f32 / h as f32) / GAME_ASPECT;

    let sig = SignatureHook {
        tag: "hud_marker_position",
        signature: MARKER_POSITION_SIG,
        offset: 9,
    };
    inject_hook(true, module, &sig, move |ctx| {
        let half = &mut ctx.xmm1.f32()[0];
        if !half.is_finite() || *half <= 0.0 {
            return;
        }
        *half *= scale;
    });
}

/// Stops world-space HUD markers being rejected once their corrected X leaves
/// the authored 1920-pixel span.
///
/// `FUN_7ff614c1d640` @ SW5.exe+29d640 projects a marker through `proj()` (the
/// function `fix_hud_marker_position` above corrects) and then gates the
/// result:
///
///     +29d78d  COMISS XMM7,XMM9                  ; XMM7 = 0.0 (XORPS)
///     +29d791  JA     +29dc8b                    ; reject if 0 > x
///     +29d797  COMISS XMM9,[R11+RBX*1+0x1748]    ; limit_x
///     +29d7a0  JA     +29dc8b                    ; reject if x > limit_x
///
/// `limit_x` is `camBlock+0x2E8`, still the authored 1920 -- neither fix above
/// touches it. So correcting a marker's X, as `fix_hud_marker_position` does,
/// moves more markers past 1920 rather than fewer, and this independent check
/// throws them away for exactly that reason. This is a different mechanism
/// from [`crate::fixes::visibility::fix_npc_visibility`] (a separate function,
/// `aec590`, gated on the projection's aspect instead) -- fixing one does not
/// fix the other.
///
/// Y still bounds the marker vertically and Z still rejects anything behind
/// the camera or past the far plane -- only the two `JA`s shown above are
/// touched.
///
/// Both `JA`s are found relative to the `COMISS` between them rather than by
/// their own bytes. A `JA rel32`'s target is a code-relative displacement: it
/// shifts if anything between the jump and +29dc8b changes size in a future
/// build, even when this branch itself is untouched. Matching a jump by its
/// own encoded target is therefore fragile in a way matching an adjacent
/// instruction is not.
///
/// `COMISS XMM9,[R11+RBX*1+0x1748]`'s encoding -- `45 0F 2F 8C 1B`, everything
/// before the trailing struct-offset immediate -- is confirmed unique
/// image-wide. The two `JA`s sit at fixed byte distances from it (6 bytes
/// before, 9 after), because they're instructions in the same unbroken
/// sequence, not independently located. Each site is then verified as `0F 87`
/// (`JA rel32`'s opcode) before being NOPed. That confirms the position
/// assumption held without requiring the jump's target to match -- the target
/// legitimately differs from what's recorded here and plays no part in
/// identifying this code.
///
/// Verified before writing either NOP: a byte patch on the wrong location --
/// particularly one that turns out to be a branch target rather than
/// ordinary code -- can crash the process outright rather than just fail
/// quietly.
fn fix_hud_marker_visibility(module: &ModuleInfo) {
    const MARKER_GATE_SIG: &str = "45 0F 2F 8C 1B";
    const NOPS: &str = "90 90 90 90 90 90";

    let Some(comiss) = pattern_scan(module.address, MARKER_GATE_SIG) else {
        log_error("hud_marker_visibility: pattern not found");
        return;
    };
    let sites = [(comiss - 6, "0 > x"), (comiss + 9, "x > limit_x")];

    for (addr, what) in sites {
        let have = unsafe { std::slice::from_raw_parts(addr as *const u8, 2) };
        if have != [0x0F, 0x87] {
            log_error(&format!(
                "hud_marker_visibility: {what} at SW5.exe+{:x} does not start with JA rel32 ({}) -- not touching it",
                addr - module.address.0 as usize,
                crate::utils::bytes_to_string(have)
            ));
            return;
        }
    }
    for (addr, _) in sites {
        crate::utils::patch(addr, NOPS);
    }
    log("hud_marker_visibility: world-space HUD markers no longer rejected past centered 16:9 boundary");
}
