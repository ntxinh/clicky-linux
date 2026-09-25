//! clicky-core: engine core for clicky-linux.
//!
//! Key identity is the HID "page:usage" string (e.g. "7:44" = Space);
//! modifiers are page-7 usages 224-231 (L/R distinct).
//! Mixer: 96 voices, trigger queue cap 1024 — render callback must never
//! alloc or lock.
//! NEVER EVIOCGRAB, never inject input, never store/transmit key text.

pub mod audio;
pub mod config;
pub mod diagnostics;
pub mod engine;
pub mod input;
pub mod keymap;
pub mod mixer;
pub mod normalizer;
pub mod profiles;
pub mod visual;
