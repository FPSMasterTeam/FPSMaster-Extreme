//! Bit-faithful replicas of Minecraft's `MathHelper` trig, which vanilla uses
//! for movement direction. Vanilla does not call `libm` sine for movement — it
//! reads a 65536-entry lookup table — and strict anti-cheats (Grim) replicate
//! that exact table. A client that uses real `sin`/`cos` drifts a little from the
//! predicted movement every tick, so for true parity the movement direction must
//! come from the same table.

use std::sync::LazyLock;

/// Degrees → radians using the exact `f32` constant vanilla multiplies by
/// (`0.017453292F`), so the table index matches Minecraft bit for bit.
pub const DEG_TO_RAD: f32 = 0.017453292;

/// `SIN_TABLE[i] = (float) sin(i * 2π / 65536)`. Built once (256 KiB).
static SIN_TABLE: LazyLock<Vec<f32>> = LazyLock::new(|| {
    (0..65536)
        .map(|i| (i as f64 * std::f64::consts::PI * 2.0 / 65536.0).sin() as f32)
        .collect()
});

/// `MathHelper.sin(value)` — `value` in radians.
#[inline]
pub fn mc_sin(value: f32) -> f32 {
    SIN_TABLE[((value * 10430.378_f32) as i32 & 0xffff) as usize]
}

/// `MathHelper.cos(value)` — `value` in radians.
#[inline]
pub fn mc_cos(value: f32) -> f32 {
    SIN_TABLE[((value * 10430.378_f32 + 16384.0_f32) as i32 & 0xffff) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_real_trig_within_table_resolution() {
        for deg in [0.0f32, 30.0, 45.0, 90.0, 137.0, 250.0, 359.0] {
            let r = deg * DEG_TO_RAD;
            assert!((mc_sin(r) - r.sin()).abs() < 1.0e-3, "sin {deg}");
            assert!((mc_cos(r) - r.cos()).abs() < 1.0e-3, "cos {deg}");
        }
    }
}
