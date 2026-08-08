//! The rain/snow curtain, ported from `EntityRenderer.renderRainSnow`.
//!
//! Vanilla draws one vertical quad per world column within a small radius of the
//! player, running from the column's precipitation height up past the camera,
//! with a scrolling texture. It is not a particle system — the "drops" are just
//! the texture, which is why heavy rain costs almost nothing.
//!
//! The one non-obvious piece is the quad's orientation. Vanilla precomputes
//! `rainXCoords` / `rainYCoords` over a 32×32 grid as `(-dz/len, dx/len)`: the
//! TANGENTIAL unit vector for that column's offset from the player, not a random
//! direction as the names suggest. Each quad therefore stands edge-on to the
//! radius and faces the player, which is what stops the curtain looking like a
//! grid of billboards.

use crate::{biome, Vertex, FULLBRIGHT};

/// One column of precipitation to draw.
#[derive(Debug, Clone, Copy)]
pub struct PrecipColumn {
    pub x: i32,
    pub z: i32,
    /// Bottom of the quad — the column's precipitation height, clamped into the
    /// band around the camera.
    pub y_min: i32,
    /// Top of the quad.
    pub y_max: i32,
    /// Snow falls slower and drifts sideways; rain is a straight fast scroll.
    pub snow: bool,
    /// Sky/block light at the column top, so the curtain is lit like the world
    /// (vanilla passes `getCombinedLight` per column).
    pub light: [f32; 2],
}

/// Vanilla's curtain radius: 10 columns on Fancy, 5 on Fast.
pub const fn curtain_radius(fancy: bool) -> i32 {
    if fancy {
        10
    } else {
        5
    }
}

/// Vanilla's per-column hash, used to decorrelate the scroll phase so every
/// column is not falling in lockstep (`renderRainSnow`'s Random seed).
fn column_hash(x: i32, z: i32) -> i32 {
    (x.wrapping_mul(x).wrapping_mul(3121))
        .wrapping_add(x.wrapping_mul(45238971))
        ^ (z.wrapping_mul(z).wrapping_mul(418711)).wrapping_add(z.wrapping_mul(13761))
}

/// A stable 0..1 value per column, standing in for vanilla's seeded `Random`
/// draws (which we cannot reproduce exactly without Java's LCG, and which only
/// decorrelate the animation).
fn column_random(x: i32, z: i32, salt: u32) -> f32 {
    let mut h = (column_hash(x, z) as u32) ^ salt.wrapping_mul(0x9E37_79B9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Which half of the returned mesh a range covers.
pub struct WeatherMesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Indices `0..rain_indices` sample `rain.png`; the rest sample `snow.png`.
    pub rain_indices: u32,
}

/// Build the curtain for this frame.
///
/// `time` is a free-running tick counter plus the frame's tick fraction — it
/// only drives the scroll phase. `strength` is the interpolated rain strength,
/// which scales every quad's alpha so weather fades in and out with the ramp.
pub fn build_mesh(
    columns: &[PrecipColumn],
    camera: [f64; 3],
    time: f32,
    strength: f32,
    radius: i32,
) -> WeatherMesh {
    let mut rain = (Vec::new(), Vec::new());
    let mut snow = (Vec::new(), Vec::new());
    let radius_f = radius.max(1) as f32;

    for column in columns {
        if column.y_min >= column.y_max {
            continue;
        }
        let (dx, dz) = (
            column.x as f64 + 0.5 - camera[0],
            column.z as f64 + 0.5 - camera[2],
        );
        // Tangential half-extent: the quad stands perpendicular to the radius, so
        // it faces the player. At the player's own column the radius is zero and
        // the direction is undefined; vanilla divides by zero there, so pick an
        // arbitrary axis instead of emitting NaNs.
        let len = ((dx * dx + dz * dz) as f32).sqrt();
        let (ox, oz) = if len > 1e-4 {
            (-(dz as f32) / len * 0.5, dx as f32 / len * 0.5)
        } else {
            (0.5, 0.0)
        };

        // Alpha falls off toward the edge of the radius so the curtain does not
        // end on a hard circle.
        let edge = (len / radius_f).min(1.0);
        let alpha = ((1.0 - edge * edge) * 0.5 + 0.5) * strength;
        if alpha <= 0.0 {
            continue;
        }

        let (top, bottom) = (column.y_max as f32, column.y_min as f32);
        // Vertical scroll. Rain falls fast (the phase advances ~3-4 tiles a
        // second); snow drifts at a small fraction of that.
        let (u0, u1, v_top, v_bottom) = if column.snow {
            let drift = column_random(column.x, column.z, 1);
            let sway = (time * 0.01 + drift * 6.28).sin() * 0.15;
            let scroll = -(time % 512.0) / 512.0 + column_random(column.x, column.z, 2);
            (
                drift + sway,
                drift + sway + 1.0,
                bottom * 0.25 + scroll,
                top * 0.25 + scroll,
            )
        } else {
            let phase = (column_hash(column.x, column.z) & 31) as f32;
            let speed = 3.0 + column_random(column.x, column.z, 3);
            let scroll = -((phase + time) / 32.0) * speed;
            (0.0, 1.0, bottom * 0.25 + scroll, top * 0.25 + scroll)
        };

        let target = if column.snow { &mut snow } else { &mut rain };
        let base = target.0.len() as u32;
        let cx = column.x as f32 + 0.5;
        let cz = column.z as f32 + 0.5;
        let color = [1.0, 1.0, 1.0, alpha];
        // Vanilla's winding: top pair first, then bottom, with the V of the top
        // taken from the BOTTOM height (and vice versa) so the texture runs
        // downward over the quad.
        for (px, pz, py, u, v) in [
            (cx - ox, cz - oz, top, u0, v_top),
            (cx + ox, cz + oz, top, u1, v_top),
            (cx + ox, cz + oz, bottom, u1, v_bottom),
            (cx - ox, cz - oz, bottom, u0, v_bottom),
        ] {
            target.0.push(Vertex {
                position: [px, py, pz],
                color,
                uv: [u, v],
                light: column.light,
            });
        }
        target.1.extend_from_slice(&[
            base,
            base + 1,
            base + 2,
            base,
            base + 2,
            base + 3,
        ]);
    }

    // Rain first, then snow, so the renderer can draw each with its own texture
    // from a single buffer.
    let rain_indices = rain.1.len() as u32;
    let vertex_split = rain.0.len() as u32;
    let mut vertices = rain.0;
    vertices.extend(snow.0);
    let mut indices = rain.1;
    indices.extend(snow.1.into_iter().map(|i| i + vertex_split));
    WeatherMesh {
        vertices,
        indices,
        rain_indices,
    }
}

/// Build the geometry for one lightning bolt, after
/// `RenderLightningBolt.doRender`.
///
/// Vanilla walks 8 segments of 16 blocks from the strike point up to y+128,
/// jittering the x/z offset at each step, and draws each segment as a square
/// tube of flat quads. A step may fork, which is what gives the bolt branches.
///
/// Two things about the shape are easy to get wrong, and I had both wrong:
///
/// * **Width.** Vanilla's `float f = 0.5F`, with vertices at `x + 0.5 ± f`, so
///   the bolt is exactly ONE BLOCK across. A tenth of that reads as a thread.
/// * **No taper.** `f` is constant over the whole bolt; it does not narrow
///   toward the ground.
///
/// Vanilla also draws the whole bolt four times over (its `k1` loop) with dim,
/// low-alpha, additively blended quads, so the brightness builds up where the
/// passes overlap and the core reads brighter than the branches. That layering
/// is reproduced here rather than faked with one opaque pass.
pub fn build_lightning(
    bolt_x: f64,
    bolt_y: f64,
    bolt_z: f64,
    seed: u32,
    brightness: f32,
) -> WeatherMesh {
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    const SEGMENTS: usize = 8;
    const SEGMENT_HEIGHT: f32 = 16.0;
    /// Vanilla `f`: half-width, so the trunk is one block across.
    const HALF_WIDTH: f32 = 0.5;
    /// Branches are drawn narrower than the trunk so the core still reads.
    const BRANCH_WIDTH: f32 = 0.25;
    /// Vanilla's bolt colour and per-pass alpha (0.45, 0.45, 0.5, 0.3).
    const COLOUR: [f32; 3] = [0.45, 0.45, 0.5];
    const PASS_ALPHA: f32 = 0.3;
    /// Vanilla's `k1` loop count — how many times the bolt is layered.
    const PASSES: u32 = 4;

    let mut emitted = 0usize;
    for pass in 0..PASSES {
        // Each pass re-walks from the same bolt seed with a different salt, so
        // the copies are near-identical but not coincident — which is what makes
        // the additive build-up look like a glow instead of a flat ribbon.
        let mut branches: Vec<(f32, f32, u32, usize, bool)> =
            vec![(0.0, 0.0, seed ^ pass.wrapping_mul(0x9E37_79B9), 0, false)];
        while let Some((mut ox, mut oz, mut rng, start, is_branch)) = branches.pop() {
            // Cap the work in case a pathological seed forks at every step.
            if emitted > 512 {
                break;
            }
            let half = if is_branch { BRANCH_WIDTH } else { HALF_WIDTH };
            for segment in start..SEGMENTS {
                let next = |r: &mut u32| {
                    *r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((*r >> 16) & 0xFFFF) as f32 / 65535.0
                };
                // Vanilla jitters by `nextInt(11) - 5` per 16-block step; scaled
                // down here so the bolt stays readable at this segment length.
                let (nx, nz) = (
                    ox + (next(&mut rng) - 0.5) * 3.0,
                    oz + (next(&mut rng) - 0.5) * 3.0,
                );
                let y0 = bolt_y as f32 + segment as f32 * SEGMENT_HEIGHT;
                let y1 = y0 + SEGMENT_HEIGHT;
                let colour = [COLOUR[0], COLOUR[1], COLOUR[2], PASS_ALPHA * brightness];

                // Square tube from (ox,oz)@y0 to (nx,nz)@y1: four corners at each
                // end, joined side by side.
                let corners = |cx: f32, cz: f32| {
                    [
                        (cx - half, cz - half),
                        (cx + half, cz - half),
                        (cx + half, cz + half),
                        (cx - half, cz + half),
                    ]
                };
                let lower = corners(ox, oz);
                let upper = corners(nx, nz);
                for side in 0..4 {
                    let next_side = (side + 1) % 4;
                    let base = vertices.len() as u32;
                    for (cx, cz, py) in [
                        (lower[side].0, lower[side].1, y0),
                        (upper[side].0, upper[side].1, y1),
                        (upper[next_side].0, upper[next_side].1, y1),
                        (lower[next_side].0, lower[next_side].1, y0),
                    ] {
                        vertices.push(Vertex {
                            position: [bolt_x as f32 + cx, py, bolt_z as f32 + cz],
                            color: colour,
                            uv: [0.0, 0.0],
                            light: FULLBRIGHT,
                        });
                    }
                    indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
                    emitted += 1;
                }

                ox = nx;
                oz = nz;
                // Fork in the middle of the run, where vanilla's branches live.
                if !is_branch && segment > 1 && segment < SEGMENTS - 1 {
                    rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    if (rng >> 16) & 0xFF < 40 {
                        branches.push((ox, oz, rng ^ 0x9E37_79B9, segment + 1, true));
                    }
                }
            }
        }
    }

    WeatherMesh {
        vertices,
        indices,
        // The bolt is drawn with its own flat-colour pass, not the rain texture.
        rain_indices: 0,
    }
}

/// Whether a column at `(x, z)` with the given biome and ground height gets
/// precipitation, and if so whether it is snow.
///
/// Mirrors `renderRainSnow`'s per-column test: dry biomes are skipped entirely,
/// and the altitude-cooled temperature decides rain vs snow.
pub fn column_precipitation(biome_id: u8, ground_y: i32) -> Option<bool> {
    if !biome::precipitates(biome_id) {
        return None;
    }
    Some(biome::temperature_at(biome_id, ground_y) < biome::SNOW_TEMPERATURE)
}

/// Full-bright light for callers that have no world light to hand.
pub const DEFAULT_LIGHT: [f32; 2] = FULLBRIGHT;

#[cfg(test)]
mod tests {
    use super::*;

    fn column(x: i32, z: i32, snow: bool) -> PrecipColumn {
        PrecipColumn {
            x,
            z,
            y_min: 64,
            y_max: 80,
            snow,
            light: DEFAULT_LIGHT,
        }
    }

    #[test]
    fn quads_face_the_player() {
        // A column due east of the camera must produce a quad whose width runs
        // north-south (perpendicular to the radius), and vice versa. The camera
        // sits at the block centre so the column really is on-axis — offset it
        // and the quad picks up a small, correct, tangential tilt.
        let mesh = build_mesh(&[column(10, 0, false)], [0.5, 64.0, 0.5], 0.0, 1.0, 10);
        let xs: Vec<f32> = mesh.vertices.iter().map(|v| v.position[0]).collect();
        let zs: Vec<f32> = mesh.vertices.iter().map(|v| v.position[2]).collect();
        let spread = |v: &[f32]| v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread(&xs) < 1e-3, "east column: no width along X");
        assert!(spread(&zs) > 0.9, "east column: width runs along Z");

        let mesh = build_mesh(&[column(0, 10, false)], [0.5, 64.0, 0.5], 0.0, 1.0, 10);
        let xs: Vec<f32> = mesh.vertices.iter().map(|v| v.position[0]).collect();
        let zs: Vec<f32> = mesh.vertices.iter().map(|v| v.position[2]).collect();
        assert!(spread(&zs) < 1e-3, "north column: no width along Z");
        assert!(spread(&xs) > 0.9, "north column: width runs along X");
    }

    #[test]
    fn the_players_own_column_does_not_produce_nans() {
        // len == 0 there; vanilla divides by zero and we must not.
        let mesh = build_mesh(&[column(0, 0, false)], [0.5, 64.0, 0.5], 0.0, 1.0, 10);
        assert!(mesh
            .vertices
            .iter()
            .all(|v| v.position.iter().all(|c| c.is_finite())));
    }

    #[test]
    fn alpha_fades_toward_the_edge_and_with_strength() {
        let near = build_mesh(&[column(1, 0, false)], [0.0, 64.0, 0.0], 0.0, 1.0, 10);
        let far = build_mesh(&[column(10, 0, false)], [0.0, 64.0, 0.0], 0.0, 1.0, 10);
        assert!(
            near.vertices[0].color[3] > far.vertices[0].color[3],
            "closer columns are more opaque"
        );
        let weak = build_mesh(&[column(1, 0, false)], [0.0, 64.0, 0.0], 0.0, 0.25, 10);
        assert!(
            weak.vertices[0].color[3] < near.vertices[0].color[3],
            "and the whole curtain scales with the rain ramp"
        );
    }

    #[test]
    fn rain_and_snow_are_split_into_separate_index_ranges() {
        let mesh = build_mesh(
            &[column(1, 0, false), column(2, 0, true), column(3, 0, false)],
            [0.0, 64.0, 0.0],
            0.0,
            1.0,
            10,
        );
        // Two rain quads then one snow quad, each 6 indices.
        assert_eq!(mesh.rain_indices, 12);
        assert_eq!(mesh.indices.len(), 18);
        // Snow indices must point past the rain vertices.
        assert!(mesh.indices[12..].iter().all(|&i| i >= 8));
        assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.vertices.len()));
    }

    #[test]
    fn degenerate_columns_are_skipped() {
        let mut c = column(1, 0, false);
        c.y_max = c.y_min;
        let mesh = build_mesh(&[c], [0.0, 64.0, 0.0], 0.0, 1.0, 10);
        assert!(mesh.vertices.is_empty());
    }

    #[test]
    fn dry_biomes_get_no_precipitation_and_cold_ones_get_snow() {
        // Desert (2) and mesa (37) have rainfall 0 -> no weather at all.
        assert_eq!(column_precipitation(2, 64), None);
        assert_eq!(column_precipitation(37, 64), None);
        // Plains rains; ice plains snows.
        assert_eq!(column_precipitation(1, 64), Some(false));
        assert_eq!(column_precipitation(12, 64), Some(true));
        // Altitude cools: taiga (0.25) crosses the 0.15 snow line high up.
        assert_eq!(column_precipitation(5, 64), Some(false));
        assert_eq!(column_precipitation(5, 200), Some(true));
    }
}

#[cfg(test)]
mod lightning_tests {
    use super::*;

    #[test]
    fn a_bolt_spans_from_the_strike_up_into_the_sky() {
        let mesh = build_lightning(100.0, 64.0, -20.0, 12345, 1.0);
        assert!(!mesh.vertices.is_empty());
        let ys: Vec<f32> = mesh.vertices.iter().map(|v| v.position[1]).collect();
        let lo = ys.iter().cloned().fold(f32::MAX, f32::min);
        let hi = ys.iter().cloned().fold(f32::MIN, f32::max);
        assert!((lo - 64.0).abs() < 1e-3, "starts at the strike point: {lo}");
        assert!(hi >= 64.0 + 8.0 * 16.0 - 1e-3, "reaches the sky: {hi}");
        // Stays near the strike column rather than wandering off.
        for v in &mesh.vertices {
            assert!((v.position[0] - 100.0).abs() < 20.0);
            assert!((v.position[2] + 20.0).abs() < 20.0);
        }
    }

    #[test]
    fn bolt_geometry_is_finite_indexed_and_deterministic() {
        let a = build_lightning(0.0, 64.0, 0.0, 777, 1.0);
        let b = build_lightning(0.0, 64.0, 0.0, 777, 1.0);
        assert_eq!(a.vertices.len(), b.vertices.len(), "same seed, same shape");
        for (x, y) in a.vertices.iter().zip(&b.vertices) {
            assert_eq!(x.position, y.position);
        }
        assert!(a.vertices.iter().all(|v| v.position.iter().all(|c| c.is_finite())));
        assert!(a.indices.iter().all(|&i| (i as usize) < a.vertices.len()));
        assert_eq!(a.indices.len() % 6, 0, "quads");

        // Different seeds diverge.
        let c = build_lightning(0.0, 64.0, 0.0, 778, 1.0);
        assert!(
            a.vertices.len() != c.vertices.len()
                || a.vertices.iter().zip(&c.vertices).any(|(x, y)| x.position != y.position)
        );
    }

    /// The bug this test exists for: the bolt was built a tenth of a block wide
    /// and tapered to a point, so it rendered as a thread. Vanilla's `f = 0.5F`
    /// makes the trunk exactly one block across, with no taper.
    #[test]
    fn the_trunk_is_one_block_wide_and_does_not_taper() {
        let mesh = build_lightning(0.0, 64.0, 0.0, 4242, 1.0);
        // Width of the cross-section at the very bottom of the bolt, where the
        // taper used to pinch it to nothing.
        let at_ground: Vec<&Vertex> = mesh
            .vertices
            .iter()
            .filter(|v| (v.position[1] - 64.0).abs() < 1e-3)
            .collect();
        assert!(!at_ground.is_empty());
        let xs: Vec<f32> = at_ground.iter().map(|v| v.position[0]).collect();
        let width = xs.iter().cloned().fold(f32::MIN, f32::max)
            - xs.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            (width - 1.0).abs() < 1e-3,
            "trunk should be one block across at the strike point, got {width}"
        );

        // And the same width partway up — no taper.
        let mid: Vec<f32> = mesh
            .vertices
            .iter()
            .filter(|v| (v.position[1] - (64.0 + 16.0)).abs() < 1e-3)
            .map(|v| v.position[0])
            .collect();
        let mid_width = mid.iter().cloned().fold(f32::MIN, f32::max)
            - mid.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            mid_width >= 1.0 - 1e-3,
            "no taper: still at least a block wide higher up, got {mid_width}"
        );
    }

    #[test]
    fn brightness_drives_the_additive_alpha() {
        let bright = build_lightning(0.0, 64.0, 0.0, 1, 1.0);
        let faded = build_lightning(0.0, 64.0, 0.0, 1, 0.25);
        // Vanilla's per-pass alpha is 0.3, scaled by the bolt's fade.
        assert!((bright.vertices[0].color[3] - 0.3).abs() < 1e-6);
        assert!((faded.vertices[0].color[3] - 0.075).abs() < 1e-6);
    }
}
