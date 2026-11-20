//! Points and rectangles in window points (top-left origin), for layout and pointer tracking.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Point {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Size {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Bounds {
    pub(crate) origin: Point,
    pub(crate) size: Size,
}

pub(crate) fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

pub(crate) fn size(width: f32, height: f32) -> Size {
    Size { width, height }
}

impl Point {
    /// Distance from the origin.
    pub(crate) fn magnitude(&self) -> f32 {
        self.x.hypot(self.y)
    }
}

impl std::ops::Add for Point {
    type Output = Point;

    fn add(self, other: Point) -> Point {
        point(self.x + other.x, self.y + other.y)
    }
}

impl std::ops::Sub for Point {
    type Output = Point;

    fn sub(self, other: Point) -> Point {
        point(self.x - other.x, self.y - other.y)
    }
}

impl Bounds {
    pub(crate) fn new(origin: Point, size: Size) -> Self {
        Self { origin, size }
    }

    pub(crate) fn contains(&self, at: &Point) -> bool {
        at.x >= self.origin.x && at.y >= self.origin.y && at.x < self.origin.x + self.size.width && at.y < self.origin.y + self.size.height
    }

    pub(crate) fn center(&self) -> Point {
        point(self.origin.x + self.size.width / 2., self.origin.y + self.size.height / 2.)
    }
}
