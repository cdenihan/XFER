use gpui::{Rgba, WindowAppearance, rgb};
#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
}
impl Theme {
    pub fn new(appearance: WindowAppearance) -> Self {
        Self {
            dark: matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        }
    }
    pub fn color(self, value: u32) -> Rgba {
        let value = match (self.dark, value) {
            (true, 0x0e141c) => 0x1b2127,
            (true, 0x151e29 | 0x101822) => 0x222a31,
            (true, 0x1c2735) => 0x29333d,
            (true, 0x63e6c9 | 0x8bedda) => 0x38a8fa,
            (true, 0x213b39 | 0x172a2b) => 0x203e55,
            (true, 0x88e5cf | 0x8ff0d8) => 0x79c7ff,
            (false, 0x0e141c) => 0xfafbfd,
            (false, 0x151e29 | 0x101822) => 0xffffff,
            (false, 0x1c2735) => 0xf1f5f9,
            (false, 0x293a4d) => 0xe9f1fa,
            (false, 0x293747 | 0x2b3a4d | 0x243141 | 0x304157) => 0xdce4ee,
            (false, 0xe6edf5 | 0xb8c9dc) => 0x182330,
            (false, 0x95a5b8 | 0x8895aa) => 0x65758b,
            (false, 0x63e6c9 | 0x8bedda) => 0x087ff5,
            (false, 0x88e5cf | 0x8ff0d8 | 0x4b9f8f) => 0x0873d8,
            (false, 0x213b39 | 0x172a2b) => 0xe7f3ff,
            (false, 0x3b2e18) => 0xfff4df,
            (false, 0xffc981) => 0x875900,
            (false, 0x422631) => 0xffedf0,
            (false, 0xffbacb) => 0xb52d47,
            (_, value) => value,
        };
        rgb(value)
    }
}
