//! evdev keycode → HID usage identity table.
//!
//! Identity is the HID "page:usage" string ("7:44" = Space); modifiers are
//! page-7 usages 224–231 (L/R distinct); mouse buttons page 9 usages 1–3.

/// (evdev code, "page:usage") — sorted by code for binary search.
static TABLE: &[(u16, &str)] = &[
    (1, "7:41"),    // KEY_ESC
    (2, "7:30"),    // KEY_1
    (3, "7:31"),    // KEY_2
    (4, "7:32"),    // KEY_3
    (5, "7:33"),    // KEY_4
    (6, "7:34"),    // KEY_5
    (7, "7:35"),    // KEY_6
    (8, "7:36"),    // KEY_7
    (9, "7:37"),    // KEY_8
    (10, "7:38"),   // KEY_9
    (11, "7:39"),   // KEY_0
    (12, "7:45"),   // KEY_MINUS
    (13, "7:46"),   // KEY_EQUAL
    (14, "7:42"),   // KEY_BACKSPACE
    (15, "7:43"),   // KEY_TAB
    (16, "7:20"),   // KEY_Q
    (17, "7:26"),   // KEY_W
    (18, "7:8"),    // KEY_E
    (19, "7:21"),   // KEY_R
    (20, "7:23"),   // KEY_T
    (21, "7:28"),   // KEY_Y
    (22, "7:24"),   // KEY_U
    (23, "7:12"),   // KEY_I
    (24, "7:18"),   // KEY_O
    (25, "7:19"),   // KEY_P
    (26, "7:47"),   // KEY_LEFTBRACE
    (27, "7:48"),   // KEY_RIGHTBRACE
    (28, "7:40"),   // KEY_ENTER
    (29, "7:224"),  // KEY_LEFTCTRL
    (30, "7:4"),    // KEY_A
    (31, "7:22"),   // KEY_S
    (32, "7:7"),    // KEY_D
    (33, "7:9"),    // KEY_F
    (34, "7:10"),   // KEY_G
    (35, "7:11"),   // KEY_H
    (36, "7:13"),   // KEY_J
    (37, "7:14"),   // KEY_K
    (38, "7:15"),   // KEY_L
    (39, "7:51"),   // KEY_SEMICOLON
    (40, "7:52"),   // KEY_APOSTROPHE
    (41, "7:53"),   // KEY_GRAVE
    (42, "7:225"),  // KEY_LEFTSHIFT
    (43, "7:49"),   // KEY_BACKSLASH
    (44, "7:29"),   // KEY_Z
    (45, "7:27"),   // KEY_X
    (46, "7:6"),    // KEY_C
    (47, "7:25"),   // KEY_V
    (48, "7:5"),    // KEY_B
    (49, "7:17"),   // KEY_N
    (50, "7:16"),   // KEY_M
    (51, "7:54"),   // KEY_COMMA
    (52, "7:55"),   // KEY_DOT
    (53, "7:56"),   // KEY_SLASH
    (54, "7:229"),  // KEY_RIGHTSHIFT
    (55, "7:85"),   // KEY_KPASTERISK
    (56, "7:226"),  // KEY_LEFTALT
    (57, "7:44"),   // KEY_SPACE
    (58, "7:57"),   // KEY_CAPSLOCK
    (59, "7:58"),   // KEY_F1
    (60, "7:59"),   // KEY_F2
    (61, "7:60"),   // KEY_F3
    (62, "7:61"),   // KEY_F4
    (63, "7:62"),   // KEY_F5
    (64, "7:63"),   // KEY_F6
    (65, "7:64"),   // KEY_F7
    (66, "7:65"),   // KEY_F8
    (67, "7:66"),   // KEY_F9
    (68, "7:67"),   // KEY_F10
    (69, "7:83"),   // KEY_NUMLOCK
    (70, "7:71"),   // KEY_SCROLLLOCK
    (71, "7:89"),   // KEY_KP7
    (72, "7:90"),   // KEY_KP8
    (73, "7:91"),   // KEY_KP9
    (74, "7:86"),   // KEY_KPMINUS
    (75, "7:92"),   // KEY_KP4
    (76, "7:93"),   // KEY_KP5
    (77, "7:94"),   // KEY_KP6
    (78, "7:87"),   // KEY_KPPLUS
    (79, "7:95"),   // KEY_KP1
    (80, "7:96"),   // KEY_KP2
    (81, "7:97"),   // KEY_KP3
    (82, "7:98"),   // KEY_KP0
    (83, "7:99"),   // KEY_KPDOT
    (86, "7:100"),  // KEY_102ND
    (87, "7:68"),   // KEY_F11
    (88, "7:69"),   // KEY_F12
    (96, "7:88"),   // KEY_KPENTER
    (97, "7:228"),  // KEY_RIGHTCTRL
    (98, "7:84"),   // KEY_KPSLASH
    (99, "7:70"),   // KEY_SYSRQ
    (100, "7:230"), // KEY_RIGHTALT
    (102, "7:74"),  // KEY_HOME
    (103, "7:82"),  // KEY_UP
    (104, "7:75"),  // KEY_PAGEUP
    (105, "7:80"),  // KEY_LEFT
    (106, "7:79"),  // KEY_RIGHT
    (107, "7:77"),  // KEY_END
    (108, "7:81"),  // KEY_DOWN
    (109, "7:78"),  // KEY_PAGEDOWN
    (110, "7:73"),  // KEY_INSERT
    (111, "7:76"),  // KEY_DELETE
    (113, "12:226"), // KEY_MUTE
    (114, "12:234"), // KEY_VOLUMEDOWN
    (115, "12:233"), // KEY_VOLUMEUP
    (116, "7:102"), // KEY_POWER
    (117, "7:103"), // KEY_KPEQUAL
    (119, "7:72"),  // KEY_PAUSE
    (121, "7:133"), // KEY_KPCOMMA
    (125, "7:227"), // KEY_LEFTMETA
    (126, "7:231"), // KEY_RIGHTMETA
    (127, "7:101"), // KEY_COMPOSE
    (163, "12:181"), // KEY_NEXTSONG
    (164, "12:205"), // KEY_PLAYPAUSE
    (165, "12:182"), // KEY_PREVIOUSSONG
    (183, "7:104"), // KEY_F13
    (184, "7:105"), // KEY_F14
    (185, "7:106"), // KEY_F15
    (186, "7:107"), // KEY_F16
    (187, "7:108"), // KEY_F17
    (188, "7:109"), // KEY_F18
    (189, "7:110"), // KEY_F19
    (190, "7:111"), // KEY_F20
    (191, "7:112"), // KEY_F21
    (192, "7:113"), // KEY_F22
    (193, "7:114"), // KEY_F23
    (194, "7:115"), // KEY_F24
    (224, "12:112"), // KEY_BRIGHTNESSDOWN
    (225, "12:111"), // KEY_BRIGHTNESSUP
    (272, "9:1"),   // BTN_LEFT
    (273, "9:2"),   // BTN_RIGHT
    (274, "9:3"),   // BTN_MIDDLE
];

fn entry(code: u16) -> Option<&'static str> {
    TABLE
        .binary_search_by_key(&code, |&(c, _)| c)
        .ok()
        .map(|i| TABLE[i].1)
}

/// HID "page:usage" identity string for an evdev code.
pub fn keyid(code: u16) -> Option<&'static str> {
    entry(code)
}

/// HID (page, usage) pair for an evdev code.
pub fn hid_usage(code: u16) -> Option<(u8, u16)> {
    let id = entry(code)?;
    let (p, u) = id.split_once(':')?;
    Some((p.parse().ok()?, u.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_mods() {
        assert_eq!(keyid(30), Some("7:4")); // KEY_A
        assert_eq!(keyid(11), Some("7:39")); // KEY_0
        assert_eq!(keyid(28), Some("7:40")); // KEY_ENTER
        assert_eq!(keyid(57), Some("7:44")); // KEY_SPACE
        assert_eq!(keyid(42), Some("7:225")); // KEY_LEFTSHIFT
        assert_eq!(keyid(54), Some("7:229")); // KEY_RIGHTSHIFT
        assert_eq!(keyid(125), Some("7:227")); // KEY_LEFTMETA
        assert_eq!(keyid(96), Some("7:88")); // KEY_KPENTER
        assert_eq!(keyid(115), Some("12:233")); // KEY_VOLUMEUP
        assert_eq!(keyid(272), Some("9:1")); // BTN_LEFT
        assert_eq!(keyid(9999), None);
    }

    #[test]
    fn table_consistent() {
        // Every row parses back to itself; table is sorted-unique.
        for w in TABLE.windows(2) {
            assert!(w[0].0 < w[1].0, "unsorted/dup at {}", w[1].0);
        }
        for &(code, id) in TABLE {
            assert_eq!(keyid(code), Some(id));
            let (p, u) = hid_usage(code).unwrap();
            assert_eq!(id, format!("{p}:{u}"));
        }
        assert_eq!(hid_usage(9999), None);
    }
}
