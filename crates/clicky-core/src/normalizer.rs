//! Normalize raw evdev EV_KEY events into `KeyEvent`s.
//!
//! Held state is tracked per-device as `(device, code)` — two keyboards can
//! hold the same key independently. `modifiers` is an 8-bit mask with one bit
//! per modifier usage (224–231 → bits 0–7), recomputed from the held set and
//! attached to every emitted event.

use std::collections::HashSet;

use crate::keymap::{hid_usage, keyid};

/// Modifier usages: page 7, 224–231 (L/R distinct). Bit index = usage − 224.
const MODIFIER_USAGES: core::ops::RangeInclusive<u16> = 224..=231;

/// Press or release phase of a normalized key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Press,
    Release,
}

/// A normalized key event ready for the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// HID "page:usage" identity string.
    pub keyid: &'static str,
    /// HID usage.
    pub usage: u16,
    /// HID usage page.
    pub page: u8,
    /// Press or release.
    pub phase: Phase,
    /// Bitmask of currently-held modifiers (bit = usage − 224).
    pub modifiers: u8,
}

/// EV_KEY normalizer. Per-device held-set provides repeat-dedup and
/// orphan-release suppression.
pub struct Normalizer {
    held: HashSet<(u32, u16)>,
}

impl Normalizer {
    pub fn new() -> Self {
        Self {
            held: HashSet::new(),
        }
    }

    /// Feed one raw event: `value` 1=press, 2=autorepeat, 0=release.
    /// Returns `None` for duplicate presses, suppressed repeats, orphan
    /// releases, and unmapped codes.
    pub fn feed(&mut self, device: u32, code: u16, value: i32, allow_repeat: bool) -> Option<KeyEvent> {
        let keyid = keyid(code)?;
        let (page, usage) = hid_usage(code)?;
        let phase = match value {
            1 => {
                if !self.held.insert((device, code)) {
                    return None; // already held
                }
                Phase::Press
            }
            2 if allow_repeat => Phase::Press,
            2 => return None,
            0 => {
                if !self.held.remove(&(device, code)) {
                    return None; // orphan release
                }
                Phase::Release
            }
            _ => return None,
        };
        Some(KeyEvent {
            keyid,
            usage,
            page,
            phase,
            modifiers: self.modifiers(),
        })
    }

    /// Drop all held state for `device` (call on SYN_DROPPED).
    pub fn clear_held(&mut self, device: u32) {
        self.held.retain(|&(d, _)| d != device);
    }

    /// Recompute the modifier bitmask from currently-held modifier usages.
    fn modifiers(&self) -> u8 {
        self.held
            .iter()
            .filter_map(|&(_, c)| hid_usage(c))
            .filter(|&(p, u)| p == 7 && MODIFIER_USAGES.contains(&u))
            .fold(0u8, |m, (_, u)| m | 1 << (u - 224))
    }
}

impl Default for Normalizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u16 = 30; // KEY_A → 7:4
    const LSHIFT: u16 = 42; // → 7:225
    const RSHIFT: u16 = 54; // → 7:229
    const DEV: u32 = 0;
    const DEV2: u32 = 1;

    #[test]
    fn press_repeat_release_orphan() {
        let mut n = Normalizer::new();
        let e = n.feed(DEV, A, 1, false).unwrap();
        assert_eq!((e.keyid, e.usage, e.page, e.phase), ("7:4", 4, 7, Phase::Press));
        // repeat suppressed without allow_repeat
        assert!(n.feed(DEV, A, 2, false).is_none());
        // duplicate press while held is deduped
        assert!(n.feed(DEV, A, 1, false).is_none());
        let e = n.feed(DEV, A, 0, false).unwrap();
        assert_eq!(e.phase, Phase::Release);
        // orphan release emits nothing
        assert!(n.feed(DEV, A, 0, false).is_none());
    }

    #[test]
    fn repeat_emits_when_allowed() {
        let mut n = Normalizer::new();
        n.feed(DEV, A, 1, false);
        let e = n.feed(DEV, A, 2, true).unwrap();
        assert_eq!(e.phase, Phase::Press);
    }

    #[test]
    fn modifier_bitmask() {
        let mut n = Normalizer::new();
        let e = n.feed(DEV, LSHIFT, 1, false).unwrap();
        assert_eq!(e.modifiers, 1 << 1); // usage 225 → bit 1
        let e = n.feed(DEV, RSHIFT, 1, false).unwrap();
        assert_eq!(e.modifiers, (1 << 1) | (1 << 5)); // 225 + 229
        // bitmask attached to every emitted event
        let e = n.feed(DEV, A, 1, false).unwrap();
        assert_eq!(e.modifiers, (1 << 1) | (1 << 5));
        n.feed(DEV, LSHIFT, 0, false);
        let e = n.feed(DEV, A, 0, false).unwrap();
        assert_eq!(e.modifiers, 1 << 5);
    }

    #[test]
    fn held_state_is_per_device() {
        let mut n = Normalizer::new();
        assert!(n.feed(DEV, A, 1, false).is_some());
        // same key on a second device is independent
        assert!(n.feed(DEV2, A, 1, false).is_some());
        assert!(n.feed(DEV, A, 0, false).is_some());
        assert!(n.feed(DEV2, A, 0, false).is_some());
    }

    #[test]
    fn clear_held_drops_device_state() {
        let mut n = Normalizer::new();
        n.feed(DEV, A, 1, false);
        n.clear_held(DEV); // SYN_DROPPED
        assert!(n.feed(DEV, A, 0, false).is_none());
        // next press emits again — not swallowed as a dup
        assert!(n.feed(DEV, A, 1, false).is_some());
        // other devices unaffected
        n.feed(DEV2, A, 1, false);
        n.clear_held(DEV);
        assert!(n.feed(DEV2, A, 0, false).is_some());
    }

    #[test]
    fn unmapped_code_emits_nothing() {
        let mut n = Normalizer::new();
        assert!(n.feed(DEV, 9999, 1, false).is_none());
        // and must not pollute held state
        assert!(n.feed(DEV, 9999, 0, false).is_none());
    }
}
