//! Integer geometry in physical pixels.

/// A rectangle in physical pixels, `left`/`top` inclusive, `right`/`bottom`
/// exclusive (as Android's `Rect` and Windows' `RECT`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn from_size(left: i32, top: i32, size: Size) -> Self {
        Self::new(left, top, left + size.width, top + size.height)
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub const fn size(&self) -> Size {
        Size::new(self.width(), self.height())
    }

    pub const fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub const fn offset(&self, dx: i32, dy: i32) -> Self {
        Self::new(
            self.left + dx,
            self.top + dy,
            self.right + dx,
            self.bottom + dy,
        )
    }

    /// `self` with `inset` removed from each side.
    pub const fn inset(&self, inset: Insets) -> Self {
        Self::new(
            self.left + inset.left,
            self.top + inset.top,
            self.right - inset.right,
            self.bottom - inset.bottom,
        )
    }

    /// A rectangle of `size` centered in `self` (rounded towards the top
    /// left).
    pub const fn center(&self, size: Size) -> Self {
        let left = self.left + (self.width() - size.width) / 2;
        let top = self.top + (self.height() - size.height) / 2;
        Self::from_size(left, top, size)
    }
}

/// A size in physical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

impl Size {
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }
}

/// Insets from each edge, in physical pixels (Android's `Insets`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Insets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Insets {
    pub const NONE: Insets = Insets::new(0, 0, 0, 0);

    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn top(top: i32) -> Self {
        Self::new(0, top, 0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_helpers() {
        let r = Rect::new(10, 20, 110, 220);
        assert_eq!(r.size(), Size::new(100, 200));
        assert!(r.contains(10, 20) && !r.contains(110, 20) && !r.contains(10, 220));
        assert_eq!(r.inset(Insets::top(32)), Rect::new(10, 52, 110, 220));
        assert_eq!(r.center(Size::new(50, 50)), Rect::new(35, 95, 85, 145));
        assert_eq!(r.offset(-10, -20), Rect::new(0, 0, 100, 200));
        assert!(Rect::new(0, 0, 0, 5).is_empty());
    }
}
