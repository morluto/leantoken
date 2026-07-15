//! Math utilities for the benchmark fixture.

/// Adds two integers.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// A point in 2D space.
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    /// Distance to another point.
    pub fn distance(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}
