//! Region types for the rendering system.
//!
//! Regions define which rows of the buffer are active and should be rendered.

/// A region of the buffer to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Starting row (inclusive).
    pub start_row: usize,
    /// Ending row (exclusive).
    pub end_row: usize,
}

impl Region {
    /// Create a new region spanning `start_row..end_row`.
    pub fn new(start_row: usize, end_row: usize) -> Self {
        Self { start_row, end_row }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_region_new() {
        let r = Region::new(2, 10);
        assert_eq!(r.start_row, 2);
        assert_eq!(r.end_row, 10);
    }
}
