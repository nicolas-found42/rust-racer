//! Core crate: authoritative vehicle simulation, scoring, and persistence.
//!
//! Pure computation over `bevy_math` types with no Bevy ECS dependencies, so
//! it is fully unit-testable and deterministic across render frame rates.
//! The Bevy layer drives this at a 120 Hz fixed step and renders the result.

pub mod persistence;
pub mod score;
pub mod sim;
pub mod tuning;

/// Simulation runs at exactly 120 Hz.
pub const FIXED_DT: f32 = 1.0 / 120.0;
/// Bounded catch-up: at most this many fixed ticks per render frame.
pub const MAX_TICKS_PER_FRAME: u32 = 8;

/// Wheel identifiers, fixed order: FL, FR, RL, RR.
pub const WHEEL_FL: usize = 0;
pub const WHEEL_FR: usize = 1;
pub const WHEEL_RL: usize = 2;
pub const WHEEL_RR: usize = 3;
pub const WHEEL_COUNT: usize = 4;

/// Lateral basis: wheels 0,1 are front (steered), 2,3 are rear (driven).
pub fn is_front(wheel: usize) -> bool {
    wheel < 2
}
pub fn is_left(wheel: usize) -> bool {
    wheel % 2 == 0
}

/// Radians <-> degrees helpers used by scoring and UI. Angle thresholds are
/// defined in degrees in the spec and converted explicitly at the boundary.
pub fn deg(rad: f32) -> f32 {
    rad * 57.29577951308232
}
pub fn rad(deg: f32) -> f32 {
    deg * 0.017453292519943295
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degree_conversion_roundtrip() {
        assert!((deg(rad(30.0)) - 30.0).abs() < 1e-5);
        assert!((deg(std::f32::consts::PI) - 180.0).abs() < 1e-3);
    }
}
