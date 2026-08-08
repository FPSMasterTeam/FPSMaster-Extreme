//! Vanilla 1.8.9 sky math: time-of-day → celestial angle, sun/star brightness,
//! sky/fog colors and the celestial (sun/moon/stars) rotation. Pure functions
//! ported from `net.minecraft.world.World` so the renderer can drive a faithful
//! day/night cycle from a single `world_time` (ticks).

use glam::Mat4;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

/// Base overworld sky color (plains biome `getSkyColorByTemp(0.8)`), the dome
/// color at full daylight before the celestial dimming factor is applied.
const BASE_SKY: [f32; 3] = [0.47, 0.655, 1.0];

/// Sun/moon quad half-size and orbit radius (vanilla `renderSky` constants).
pub const SUN_SIZE: f32 = 30.0;
pub const MOON_SIZE: f32 = 20.0;
pub const CELESTIAL_DIST: f32 = 100.0;

/// Vanilla `World.getCelestialAngle`: maps world time to a 0..1 sky rotation,
/// with the easing that holds the sun a touch longer near the horizon.
pub fn celestial_angle(time_ticks: f64) -> f32 {
    let i = time_ticks.rem_euclid(24000.0);
    let mut f = (i / 24000.0) as f32 - 0.25;
    if f < 0.0 {
        f += 1.0;
    }
    if f > 1.0 {
        f -= 1.0;
    }
    let f1 = 1.0 - ((f * PI).cos() + 1.0) / 2.0;
    f + (f1 - f) / 3.0
}

/// Vanilla `World.getSunBrightness` (clear weather): 0.2 at midnight, 1.0 at
/// noon. Used as the sky-light scale in the lightmap so caves/torch-lit areas
/// stay lit while open ground darkens at night.
pub fn sun_brightness(time_ticks: f64) -> f32 {
    let angle = celestial_angle(time_ticks);
    let mut f1 = 1.0 - ((angle * TAU).cos() * 2.0 + 0.5);
    f1 = f1.clamp(0.0, 1.0);
    f1 = 1.0 - f1;
    f1 * 0.8 + 0.2
}

/// Vanilla `World.getStarBrightness` (clear weather): 0 by day, up to 0.5 at
/// midnight. Drives the star quads' alpha.
pub fn star_brightness(time_ticks: f64) -> f32 {
    let angle = celestial_angle(time_ticks);
    let mut f1 = 1.0 - ((angle * TAU).cos() * 2.0 + 0.25);
    f1 = f1.clamp(0.0, 1.0);
    f1 * f1 * 0.5
}

/// Vanilla moon phase 0..7 (`World.getMoonPhase`), selecting the tile in
/// `moon_phases.png`.
pub fn moon_phase(time_ticks: f64) -> u32 {
    (time_ticks.div_euclid(24000.0) as i64).rem_euclid(8) as u32
}

/// sRGB -> linear, for the vanilla colour constants below.
///
/// Vanilla's sky/fog values are authored for a non-sRGB framebuffer — they are the
/// bytes it puts on screen. Every consumer here (the sky pass, the terrain fog mix,
/// the HDR clear colour) writes into the LINEAR `Rgba16Float` world target that the
/// post pass re-encodes to sRGB, so handing them over raw renders them washed out
/// (vanilla's 0.47 daytime sky blue would display as ~0.71 — pale and milky).
/// Convert once here, at the boundary, so the whole renderer stays linear.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Vanilla's per-level brightness table (`WorldProvider.lightBrightnessTable`
/// with ambient 0): `l / (4 - 3l)`. The same curve `chunk.wgsl`'s `light_level`
/// applies per fragment.
fn light_level(level: f32) -> f32 {
    let l = level.clamp(0.0, 1.0);
    l / (4.0 - 3.0 * l)
}

/// Vanilla's coloured light map for one (sky, block) light pair, 0..15 each —
/// the CPU twin of `chunk.wgsl`'s `vanilla_lightmap` + `light_curve`, ported
/// from `EntityRenderer.updateLightmap`.
///
/// Entities need the identical result: vanilla samples one lightmap texel per
/// entity, so a mob standing on a torch-lit floor must take the same warm colour
/// as the floor. Anything else and mobs visibly disagree with the block they are
/// standing on.
///
/// Returns a GAMMA-space multiplier, exactly like the shader's — the caller
/// converts to linear before it scales a linear texel.
pub fn vanilla_lightmap(sky_level: u8, block_level: u8, sun_brightness: f32, gamma: f32) -> [f32; 3] {
    let sun = sun_brightness;
    let sky = light_level(sky_level as f32 / 15.0) * (sun * 0.95 + 0.05);
    // The block term's `* 1.5` gain is vanilla's torch-flicker base; the flicker
    // itself is a per-frame random walk the shader fakes, and an entity taking
    // the un-flickered mean is imperceptible.
    let block = light_level(block_level as f32 / 15.0) * 1.5;
    let sky_rg = sky * (sun * 0.65 + 0.35);
    let block_g = block * ((block * 0.6 + 0.4) * 0.6 + 0.4);
    let block_b = block * (block * block * 0.6 + 0.4);
    let mut rgb = [sky_rg + block, sky_rg + block_g, sky + block_b];
    for c in rgb.iter_mut() {
        *c = (*c * 0.96 + 0.03).clamp(0.0, 1.0);
        // Brightness gamma: blend toward the lifted curve, then vanilla repeats
        // the lift before writing the lightmap texture.
        let lifted = 1.0 - (1.0 - *c).powi(4);
        *c = *c * (1.0 - gamma) + lifted * gamma;
        *c = (*c * 0.96 + 0.03).clamp(0.0, 1.0);
    }
    rgb
}

/// The full set of time-dependent sky parameters the renderer needs in one
/// shot, so a frame computes the celestial math once.
///
/// All colours are LINEAR (see [`srgb_to_linear`]); the scalars are not colours
/// and are passed through untouched.
#[derive(Debug, Clone, Copy)]
pub struct SkyColors {
    /// Upper-dome color (vanilla `getSkyColor`): deep blue by day, black at night.
    pub zenith: [f32; 3],
    /// Horizon/fog color (vanilla `getFogColor`): lighter, drives the gradient
    /// near the horizon and the terrain clear color.
    pub horizon: [f32; 3],
    /// Sunrise/sunset glow (vanilla `calcSunriseSunsetColors`): rgb + strength;
    /// strength is 0 away from dawn/dusk. Only the rgb is linearized — `[3]` is a
    /// blend weight, not a colour.
    pub sunset: [f32; 4],
    /// Sky-light scale fed to the world lightmap (== `sun_brightness`).
    pub sun_brightness: f32,
    /// Star quad alpha (== `star_brightness`).
    pub star_brightness: f32,
}

pub fn sky_colors(time_ticks: f64) -> SkyColors {
    let angle = celestial_angle(time_ticks);
    // Celestial dimming factor shared by the sky and fog colors.
    let f = ((angle * TAU).cos() * 2.0 + 0.5).clamp(0.0, 1.0);

    // The vanilla math runs in vanilla's own (gamma) space, exactly as written in
    // `World.getSkyColor` / `getFogColor` — including the dimming factor `f`, which
    // is a gamma-space multiplier. Only the finished colour is converted to linear.
    let zenith = [BASE_SKY[0] * f, BASE_SKY[1] * f, BASE_SKY[2] * f];
    let horizon = [
        0.752_941_2 * (f * 0.94 + 0.06),
        0.847_058_83 * (f * 0.94 + 0.06),
        1.0 * (f * 0.91 + 0.09),
    ];
    let sunset = sunrise_sunset_color(angle);

    SkyColors {
        zenith: zenith.map(srgb_to_linear),
        horizon: horizon.map(srgb_to_linear),
        sunset: [
            srgb_to_linear(sunset[0]),
            srgb_to_linear(sunset[1]),
            srgb_to_linear(sunset[2]),
            sunset[3],
        ],
        sun_brightness: sun_brightness(time_ticks),
        star_brightness: star_brightness(time_ticks),
    }
}

/// Vanilla `calcSunriseSunsetColors`: an orange horizon glow that fades in only
/// while the sun is near the horizon. Returns rgb + strength (0 = no glow).
fn sunrise_sunset_color(angle: f32) -> [f32; 4] {
    let f1 = (angle * TAU).cos();
    if !(-0.4..=0.4).contains(&f1) {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let f3 = (f1 / 0.4) * 0.5 + 0.5;
    let mut f4 = 1.0 - (1.0 - (f3 * PI).sin()) * 0.99;
    f4 *= f4;
    [f3 * 0.3 + 0.7, f3 * f3 * 0.7 + 0.2, 0.2, f4]
}

/// The celestial-sphere rotation for `time_ticks` (vanilla `renderSky`:
/// `rotate(-90, Y)` then `rotate(angle*360, X)`). Sun/moon/star geometry is
/// authored in this local frame and transformed by it so the sky wheels east to
/// west with the day.
pub fn celestial_rotation(time_ticks: f64) -> Mat4 {
    let angle = celestial_angle(time_ticks);
    Mat4::from_rotation_y(-FRAC_PI_2) * Mat4::from_rotation_x(angle * TAU)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(got: [f32; 3], want: [f32; 3]) {
        for c in 0..3 {
            assert!(
                (got[c] - want[c]).abs() < 0.002,
                "channel {c}: got {got:?}, want {want:?}"
            );
        }
    }

    /// These are the values `chunk.wgsl`'s `vanilla_lightmap_graded` produces for
    /// the same inputs. Entities are lit by this CPU copy and terrain by the
    /// shader, so the two must not drift apart.
    #[test]
    fn cpu_lightmap_matches_the_shader_formula() {
        // Noon under open sky: essentially full white.
        approx(vanilla_lightmap(15, 0, 1.0, 0.0), [0.980, 0.980, 0.980]);
        // Indoors by a dim torch: warm, red > green > blue.
        approx(vanilla_lightmap(0, 7, 1.0, 0.0), [0.307, 0.242, 0.169]);
        // Midnight under open sky: dim and distinctly blue.
        approx(vanilla_lightmap(15, 0, 0.2, 0.0), [0.165, 0.165, 0.280]);
        // Unlit cave: vanilla's floor, not pure black.
        approx(vanilla_lightmap(0, 0, 1.0, 0.0), [0.059, 0.059, 0.059]);
    }

    #[test]
    fn torch_light_is_warm_at_low_levels_and_whitens_near_the_source() {
        let dim = vanilla_lightmap(0, 5, 1.0, 0.0);
        assert!(dim[0] > dim[1] && dim[1] > dim[2], "dim torch is orange: {dim:?}");
        let close = vanilla_lightmap(0, 15, 1.0, 0.0);
        assert!(
            (close[0] - close[2]).abs() < 0.01,
            "right at the source it washes to white: {close:?}"
        );
    }

    #[test]
    fn brightness_gamma_only_lifts_the_dark_end() {
        let moody = vanilla_lightmap(4, 0, 1.0, 0.0);
        let bright = vanilla_lightmap(4, 0, 1.0, 1.0);
        assert!(bright[0] > moody[0], "Bright lifts a dim sample");
        let noon_moody = vanilla_lightmap(15, 0, 1.0, 0.0);
        let noon_bright = vanilla_lightmap(15, 0, 1.0, 1.0);
        assert!(
            noon_bright[0] >= noon_moody[0] - 0.001,
            "and never darkens a bright one"
        );
    }
}
