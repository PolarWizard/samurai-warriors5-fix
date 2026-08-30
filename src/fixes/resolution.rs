//! The resolution list the Graphics menu offers.

use std::sync::atomic::Ordering;

use crate::utils::{self, ModuleInfo, log, log_error};
use crate::{RENDER_HEIGHT, RENDER_WIDTH};

/// Forces every selectable resolution in the Graphics menu to the resolved
/// width/height, so whichever entry the game has stored as "last selected" --
/// including one saved from before this ran -- always ends up applying the
/// same value rather than depending on which entry that happens to be.
///
/// Width and height are two separate lookup tables, not paired structs: one
/// function at SW5.exe+0xfcd0 returns a width for an index 0..10, a second at
/// SW5.exe+0xfd50 returns a height for the same index, and other code presumably
/// calls both with whichever index is currently selected. Each builds its
/// table as eleven `mov [rax+disp8], imm32` stores (opcode + ModRM + disp8 +
/// imm32, 7 bytes each) against the pre-adjustment stack pointer, so scanning
/// for the exact bytes of a known entry's store finds that one instruction
/// uniquely, and rewriting just the trailing 4-byte immediate changes only
/// what that entry returns.
///
/// Index 0 in both tables stores 0, not a resolution -- almost certainly a
/// sentinel some other code path checks for (this project's own `RENDER_WIDTH`
/// uses the same convention), rather than a real selectable entry. Left alone
/// for that reason. Indices 1..10 are a uniform 16:9 ladder from 640x360 to
/// 7680x4320 -- ten real entries, all patched here, both tables.
pub fn fix_resolution_list(module: &ModuleInfo) {
    let (w, h) = (
        RENDER_WIDTH.load(Ordering::Relaxed),
        RENDER_HEIGHT.load(Ordering::Relaxed),
    );

    // (disp8, native immediate) -- indices 1..10, low to high, same order in
    // both tables.
    const SLOTS: [(u8, [u8; 4]); 10] = [
        (0xCC, [0x80, 0x02, 0x00, 0x00]), // 640
        (0xD0, [0x20, 0x03, 0x00, 0x00]), // 800
        (0xD4, [0x00, 0x04, 0x00, 0x00]), // 1024
        (0xD8, [0x00, 0x05, 0x00, 0x00]), // 1280
        (0xDC, [0x50, 0x05, 0x00, 0x00]), // 1360
        (0xE0, [0x40, 0x06, 0x00, 0x00]), // 1600
        (0xE4, [0x80, 0x07, 0x00, 0x00]), // 1920
        (0xE8, [0x00, 0x0A, 0x00, 0x00]), // 2560
        (0xEC, [0x00, 0x0F, 0x00, 0x00]), // 3840
        (0xF0, [0x00, 0x1E, 0x00, 0x00]), // 7680
    ];
    const HEIGHT_SLOTS: [(u8, [u8; 4]); 10] = [
        (0xCC, [0x68, 0x01, 0x00, 0x00]), // 360
        (0xD0, [0xC2, 0x01, 0x00, 0x00]), // 450
        (0xD4, [0x40, 0x02, 0x00, 0x00]), // 576
        (0xD8, [0xD0, 0x02, 0x00, 0x00]), // 720
        (0xDC, [0xFD, 0x02, 0x00, 0x00]), // 765
        (0xE0, [0x84, 0x03, 0x00, 0x00]), // 900
        (0xE4, [0x38, 0x04, 0x00, 0x00]), // 1080
        (0xE8, [0xA0, 0x05, 0x00, 0x00]), // 1440
        (0xEC, [0x70, 0x08, 0x00, 0x00]), // 2160
        (0xF0, [0xE0, 0x10, 0x00, 0x00]), // 4320
    ];

    apply(module, "resX", &SLOTS, w);
    apply(module, "resY", &HEIGHT_SLOTS, h);
}

fn apply(module: &ModuleInfo, label: &str, slots: &[(u8, [u8; 4]); 10], value: u32) {
    for (disp8, native_imm) in slots {
        let find = format!("C7 40 {disp8:02X} {}", utils::bytes_to_string(native_imm));
        let replace = format!("C7 40 {disp8:02X} {}", utils::bytes_to_string(&value.to_le_bytes()));
        match utils::pattern_scan_all(module.address, &find) {
            hits if hits.is_empty() => {
                log_error(&format!("ultrawide: {label} disp {disp8:02X} -- pattern not found"));
            }
            hits => {
                for addr in &hits {
                    utils::patch(*addr, &replace);
                }
                let rel = hits[0] - module.address.0 as usize;
                log(&format!(
                    "ultrawide: {label} disp {disp8:02X} -> {value} at {}+{rel:x}{}",
                    module.name,
                    if hits.len() > 1 { format!(" (+{} more)", hits.len() - 1) } else { String::new() }
                ));
            }
        }
    }
}
