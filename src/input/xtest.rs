use anyhow::{Context, Result};
use std::collections::HashSet;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, Keycode};
use x11rb::protocol::xtest::ConnectionExt as XtestExt;
use x11rb::rust_connection::RustConnection;

use super::InputBackend;

const FAKE_KEY_PRESS: u8 = 2;
const FAKE_KEY_RELEASE: u8 = 3;
const FAKE_BUTTON_PRESS: u8 = 4;
const FAKE_BUTTON_RELEASE: u8 = 5;
const FAKE_MOTION: u8 = 6;
/// XTEST detail for motion: 0 = absolute, 1 = relative.
const MOTION_RELATIVE: u8 = 1;

/// X11 XTest injection.  Works on every X11 session; on XWayland it reaches
/// XWayland-native windows only, which is why the portal backend is
/// preferred on Wayland-only sessions.
pub struct XtestBackend {
    conn: RustConnection,
    screen: usize,
    held_keys: HashSet<Keycode>,
    held_buttons: HashSet<u8>,
}

impl XtestBackend {
    pub fn connect() -> Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("connecting to X display")?;
        Ok(Self {
            conn,
            screen,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
        })
    }

    fn root(&self) -> x11rb::protocol::xproto::Window {
        self.conn.setup().roots[self.screen].root
    }

    fn screen_size(&self) -> (i16, i16) {
        let root = &self.conn.setup().roots[self.screen];
        (root.width_in_pixels as i16, root.height_in_pixels as i16)
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<()> {
        self.conn
            .xtest_fake_input(kind, detail, 0, self.root(), x, y, 0)
            .context("XTestFakeInput")?;
        self.conn.flush()?;
        Ok(())
    }
}

impl InputBackend for XtestBackend {
    fn name(&self) -> &'static str {
        "xtest"
    }

    fn key(&mut self, vkey: u16, down: bool) -> Result<()> {
        let keycode = self.vkey_to_keycode(vkey)?;
        self.fake(
            if down {
                FAKE_KEY_PRESS
            } else {
                FAKE_KEY_RELEASE
            },
            keycode,
            0,
            0,
        )?;
        if down {
            self.held_keys.insert(keycode);
        } else {
            self.held_keys.remove(&keycode);
        }
        Ok(())
    }

    fn button(&mut self, button: u16, down: bool) -> Result<()> {
        // Windows VK numbering (1 left, 2 right, 3 middle) differs from X11
        // (1 left, 2 middle, 3 right); XBUTTON 8/9 agree on both sides.
        let xbutton: u16 = match button {
            2 => 3,
            3 => 2,
            other => other,
        };
        self.fake(
            if down {
                FAKE_BUTTON_PRESS
            } else {
                FAKE_BUTTON_RELEASE
            },
            xbutton as u8,
            0,
            0,
        )?;
        if down {
            self.held_buttons.insert(xbutton as u8);
        } else {
            self.held_buttons.remove(&(xbutton as u8));
        }
        Ok(())
    }

    fn motion(&mut self, absolute: bool, x: i32, y: i32) -> Result<()> {
        if absolute {
            let (width, height) = self.screen_size();
            let clamped_x = ((x.clamp(0, 65535) as i64 * (width as i64 - 1)) / 65535) as i16;
            let clamped_y = ((y.clamp(0, 65535) as i64 * (height as i64 - 1)) / 65535) as i16;
            self.fake(FAKE_MOTION, 0, clamped_x, clamped_y)?;
        } else {
            self.fake(FAKE_MOTION, MOTION_RELATIVE, x as i16, y as i16)?;
        }
        Ok(())
    }

    fn wheel(&mut self, horizontal: bool, delta: i32) -> Result<()> {
        // X11 has no wheel concept: synthesize button 4/5 (vertical) and
        // 6/7 (horizontal) presses, one per notch.
        let (up_button, down_button) = if horizontal { (7u8, 6u8) } else { (4u8, 5u8) };
        let steps = super::wheel_steps(delta);
        for _ in 0..steps.unsigned_abs() {
            let button = if steps > 0 { up_button } else { down_button };
            self.fake(FAKE_BUTTON_PRESS, button, 0, 0)?;
            self.fake(FAKE_BUTTON_RELEASE, button, 0, 0)?;
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<()> {
        for keycode in self.held_keys.clone() {
            self.fake(FAKE_KEY_RELEASE, keycode, 0, 0)?;
        }
        self.held_keys.clear();
        for button in self.held_buttons.clone() {
            self.fake(FAKE_BUTTON_RELEASE, button, 0, 0)?;
        }
        self.held_buttons.clear();
        self.conn.flush()?;
        Ok(())
    }
}

impl XtestBackend {
    /// Map a Windows virtual-key code onto this server's keymap via the
    /// invariant keysym subset.  Layout-aware remapping for non-representable
    /// keys is Phase 2 together with the semantic text path.
    fn vkey_to_keycode(&self, vkey: u16) -> Result<Keycode> {
        let keysym = vkey_to_keysym(vkey);

        let setup = self.conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        let map = self
            .conn
            .get_keyboard_mapping(min, max - min + 1)?
            .reply()?;

        if keysym != 0 {
            let per = map.keysyms_per_keycode.max(1) as usize;
            for (index, chunk) in map.keysyms.chunks(per).enumerate() {
                if chunk.contains(&keysym) {
                    return Ok(min + index as Keycode);
                }
            }
        }

        // Never inject an unmapped key as a raw keycode: the Windows VK
        // value is not an X keycode and would press an arbitrary key.
        anyhow::bail!("Windows virtual key 0x{vkey:02x} has no keysym on this keymap")
    }
}

/// Windows virtual key -> X11 keysym.  Pure so it can be unit-tested
/// without a server connection.  `vkey` may carry the 0x100 extended flag
/// set by the Windows hook.
fn vkey_to_keysym(vkey: u16) -> u32 {
    let base = (vkey & 0xff) as u32;

    match base {
        0x03 => 0xff6b,                        // Break (Ctrl+Break, VK_CANCEL)
        0x08 => 0xff08,                        // BackSpace
        0x09 => 0xff09,                        // Tab
        0x0c => 0xff0b,                        // Clear (keypad center)
        0x0d if vkey & 0x100 != 0 => 0xff8d,   // KP Enter (extended Return)
        0x0d => 0xff0d,                        // Return
        0x10 if vkey & 0x100 != 0 => 0xffe2,   // Shift R (extended VK_SHIFT)
        0x10 => 0xffe1,                        // Shift L (base VK_SHIFT)
        0x11 if vkey & 0x100 != 0 => 0xffe4,   // Control R (extended VK_CONTROL)
        0x11 => 0xffe3,                        // Control L (base VK_CONTROL)
        0x12 if vkey & 0x100 != 0 => 0xffea,   // Alt R (extended VK_MENU)
        0x12 => 0xffe9,                        // Alt L (base VK_MENU)
        0x13 => 0xff13,                        // Pause
        0x14 => 0xffe5,                        // Caps Lock
        0x1b => 0xff1b,                        // Escape
        0x20 => 0x0020,                        // Space
        0x21 => 0xff55,                        // Prior
        0x22 => 0xff56,                        // Next
        0x23 => 0xff57,                        // End
        0x24 => 0xff50,                        // Home
        0x25 => 0xff51,                        // Left
        0x26 => 0xff52,                        // Up
        0x27 => 0xff53,                        // Right
        0x28 => 0xff54,                        // Down
        0x29 => 0xff60,                        // Select
        0x2b => 0xff62,                        // Execute
        0x2c if vkey & 0x100 != 0 => 0xff69,   // Sys Req (extended Print)
        0x2c => 0xff61,                        // Print
        0x2d => 0xff63,                        // Insert
        0x2e => 0xffff,                        // Delete (XK_Delete, NOT KP_Delete:
                                               // 0xff9f lives on the KP-dot keycode
                                               // and would type ".")
        0x2f => 0xff6a,                        // Help
        0x30..=0x39 => base,                   // 0-9
        0x41..=0x5a => base + 0x20,            // A-Z -> lowercase keysyms
        0x5b => 0xffeb,                        // Meta L
        0x5c => 0xffec,                        // Meta R
        0x5d => 0xff67,                        // Menu
        0x5f => 0x1008ff2f,                    // Sleep (XF86XK_Sleep)
        0x60..=0x69 => 0xffb0 + (base - 0x60), // KP_0..KP_9
        0x6a => 0xffaa,                        // KP multiply
        0x6b => 0xffab,                        // KP add
        0x6c => 0xffa5,                        // KP separator
        0x6d => 0xffad,                        // KP subtract
        0x6e => 0xffae,                        // KP decimal
        0x6f => 0xffaf,                        // KP divide
        0x70..=0x87 => 0xffbe + (base - 0x70), // F1..F24
        0x90 => 0xff7f,                        // Num Lock
        0x91 => 0xff14,                        // Scroll Lock
        0xa0 => 0xffe1,                        // Shift L
        0xa1 => 0xffe2,                        // Shift R
        0xa2 => 0xffe3,                        // Control L
        0xa3 => 0xffe4,                        // Control R
        0xa4 => 0xffe9,                        // Alt L
        0xa5 => 0xffea,                        // Alt R
        0xa6 => 0x1008ff26,                    // Browser back
        0xa7 => 0x1008ff27,                    // Browser forward
        0xa8 => 0x1008ff29,                    // Browser refresh
        0xa9 => 0x1008ff28,                    // Browser stop
        0xaa => 0x1008ff1b,                    // Browser search
        0xab => 0x1008ff30,                    // Browser favorites
        0xac => 0x1008ff18,                    // Browser home
        0xad => 0x1008ff12,                    // Volume mute
        0xae => 0x1008ff11,                    // Volume down
        0xaf => 0x1008ff13,                    // Volume up
        0xb0 => 0x1008ff17,                    // Next track
        0xb1 => 0x1008ff16,                    // Previous track
        0xb2 => 0x1008ff15,                    // Stop media
        0xb3 => 0x1008ff14,                    // Play/pause media
        0xb4 => 0x1008ff19,                    // Launch mail
        0xb5 => 0x1008ff32,                    // Launch media player
        0xb6 => 0x1008ff40,                    // Launch application 1
        0xb7 => 0x1008ff41,                    // Launch application 2
        0xba => 0x3b,                          // ;
        0xbb => 0x3d,                          // =
        0xbc => 0x2c,                          // ,
        0xbd => 0x2d,                          // -
        0xbe => 0x2e,                          // .
        0xbf => 0x2f,                          // /
        0xc0 => 0x60,                          // `
        0xdb => 0x5b,                          // [
        0xdc => 0x5c,                          // backslash
        0xdd => 0x5d,                          // ]
        0xde => 0x27,                          // '
        0xe2 => 0x5c,                          // OEM_102 (ISO key next to LShift)
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::vkey_to_keysym;

    #[test]
    fn print_screen_and_sysrq_have_distinct_keysyms() {
        assert_eq!(vkey_to_keysym(0x2c), 0xff61); // XK_Print
        assert_eq!(vkey_to_keysym(0x12c), 0xff69); // XK_Sys_Req
    }

    #[test]
    fn insert_and_delete_map_to_their_own_keysyms() {
        // XK_Insert = 0xff63, XK_Delete = 0xffff.  The historical bugs were:
        // Insert -> 0xffff (Delete's keysym) and Delete -> 0xff9f
        // (KP_Delete, whose keycode is the KP-dot key and types ".").
        assert_eq!(vkey_to_keysym(0x2d), 0xff63);
        assert_eq!(vkey_to_keysym(0x2e), 0xffff);
        assert_ne!(vkey_to_keysym(0x2d), vkey_to_keysym(0x2e));
    }

    #[test]
    fn snapshot_is_not_silently_identity_mapped() {
        // Regression: the old code had no 0x2c entry at all, and the
        // fallback injected the VK value 0x2c (44) as an X keycode, which
        // is evdev KEY_J on the standard keymap.
        assert_ne!(vkey_to_keysym(0x2c), 0);
    }

    #[test]
    fn alphabet_maps_to_lowercase_keysyms() {
        assert_eq!(vkey_to_keysym(0x4a), 'j' as u32);
        assert_eq!(vkey_to_keysym(0x41), 'a' as u32);
    }

    #[test]
    fn base_modifier_codes_map_like_the_explicit_left_codes() {
        // uinput's evdev table accepts both the base VKs (0x10-0x12) and the
        // L/R codes (0xa0-0xa5); the xtest table must not diverge.
        assert_eq!(vkey_to_keysym(0x10), vkey_to_keysym(0xa0));
        assert_eq!(vkey_to_keysym(0x11), vkey_to_keysym(0xa2));
        assert_eq!(vkey_to_keysym(0x12), vkey_to_keysym(0xa4));
        assert_eq!(vkey_to_keysym(0x110), 0xffe2); // Shift R
        assert_eq!(vkey_to_keysym(0x111), 0xffe4); // Control R
        assert_eq!(vkey_to_keysym(0x112), 0xffea); // Alt R
    }

    #[test]
    fn extended_return_is_keypad_enter() {
        assert_eq!(vkey_to_keysym(0x10d), 0xff8d);
        assert_eq!(vkey_to_keysym(0x0d), 0xff0d);
    }

    #[test]
    fn media_and_volume_keys_use_xf86_keysyms() {
        assert_eq!(vkey_to_keysym(0xad), 0x1008ff12); // mute
        assert_eq!(vkey_to_keysym(0xaf), 0x1008ff13); // volume up
        assert_eq!(vkey_to_keysym(0xb0), 0x1008ff17); // next track
        assert_eq!(vkey_to_keysym(0xb3), 0x1008ff14); // play/pause
        assert_eq!(vkey_to_keysym(0x5f), 0x1008ff2f); // sleep
    }
}
