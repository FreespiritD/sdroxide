//! Waterfall colormap LUTs: 256×1 RGBA8.

pub const NAMES: [&str; 8] =
    ["Classic", "Viridis", "Gray", "Icom", "Neon", "Synthwave", "Matrix", "Tron"];

/// Piecewise-linear gradient through (position, RGB) anchor points.
/// Anchors must start at 0.0 and end at 1.0.
fn gradient(anchors: &[(f32, [u8; 3])]) -> [u8; 256 * 4] {
    let mut out = [0u8; 256 * 4];
    for i in 0..256 {
        let t = i as f32 / 255.0;
        let seg = anchors.windows(2).find(|w| t <= w[1].0).unwrap_or(&anchors[anchors.len() - 2..]);
        let (t0, c0) = seg[0];
        let (t1, c1) = seg[1];
        let f = if t1 > t0 { ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { 0.0 };
        for ch in 0..3 {
            out[i * 4 + ch] = (c0[ch] as f32 + f * (c1[ch] as f32 - c0[ch] as f32)) as u8;
        }
        out[i * 4 + 3] = 255;
    }
    out
}

pub fn lut(index: usize) -> [u8; 256 * 4] {
    match index {
        // PowerSDR-style: black → blue → cyan → green → yellow → red → white
        0 => gradient(&[
            (0.00, [0, 0, 0]),
            (0.25, [0, 0, 160]),
            (0.45, [0, 180, 200]),
            (0.60, [40, 200, 60]),
            (0.75, [230, 230, 40]),
            (0.90, [240, 60, 30]),
            (1.00, [255, 255, 255]),
        ]),
        // Viridis approximation
        1 => gradient(&[
            (0.00, [68, 1, 84]),
            (0.25, [59, 82, 139]),
            (0.50, [33, 145, 140]),
            (0.75, [94, 201, 98]),
            (1.00, [253, 231, 37]),
        ]),
        // Icom SDR waterfall: floor black, rising through blue → cyan → green →
        // yellow → orange, peaking at red (no white blow-out at the top).
        3 => gradient(&[
            (0.00, [0, 0, 0]),
            (0.12, [0, 0, 92]),
            (0.30, [0, 40, 210]),
            (0.46, [0, 170, 220]),
            (0.58, [0, 210, 180]),
            (0.70, [40, 210, 40]),
            (0.83, [235, 235, 30]),
            (0.93, [242, 130, 20]),
            (1.00, [230, 20, 20]),
        ]),
        // Neon — cyberpunk magenta-and-cyan glow: black → violet → magenta →
        // hot pink → neon cyan → white.
        4 => gradient(&[
            (0.00, [0, 0, 0]),
            (0.18, [24, 0, 48]),
            (0.38, [96, 0, 140]),
            (0.56, [210, 0, 190]),
            (0.72, [255, 44, 130]),
            (0.86, [70, 220, 255]),
            (1.00, [235, 255, 255]),
        ]),
        // Synthwave — retro-future sunset: deep indigo → purple → magenta →
        // coral → orange → hot yellow.
        5 => gradient(&[
            (0.00, [8, 0, 20]),
            (0.22, [58, 0, 92]),
            (0.42, [150, 12, 130]),
            (0.60, [240, 40, 110]),
            (0.75, [255, 96, 74]),
            (0.88, [255, 158, 44]),
            (1.00, [255, 232, 120]),
        ]),
        // Matrix — green phosphor rain: black → dim green → green → bright
        // green → pale green.
        6 => gradient(&[
            (0.00, [0, 0, 0]),
            (0.30, [0, 36, 8]),
            (0.55, [0, 150, 40]),
            (0.78, [46, 240, 88]),
            (1.00, [200, 255, 205]),
        ]),
        // Tron — electric grid: black → deep blue → cyan → white, spiking to an
        // amber peak.
        7 => gradient(&[
            (0.00, [0, 0, 0]),
            (0.28, [0, 18, 58]),
            (0.52, [0, 168, 232]),
            (0.72, [120, 238, 255]),
            (0.86, [244, 252, 255]),
            (1.00, [255, 150, 26]),
        ]),
        // Gray (index 2) and any out-of-range fallback.
        _ => gradient(&[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])]),
    }
}

// ── Propagation ─────────────────────────────────────────────────────────────

/// The propagation heat ramp: blue → green → yellow → red.
///
/// Not one of the waterfall palettes. Those start at black because a waterfall
/// is mostly noise floor and the empty parts should recede; a propagation cell
/// with no evidence is drawn with no alpha at all rather than in black, so this
/// ramp spends its whole range on the four steps an operator reads as "barely",
/// "workable", "good", "loud".
///
/// This is the one definition of that ramp. The globe samples it as a 256×1
/// texture and the flat map indexes the same array on the CPU, so the two
/// cannot drift apart — which they would within a week if the anchors were
/// written out again in WGSL.
pub fn prop_ramp() -> [u8; 256 * 4] {
    gradient(&[
        (0.00, [26, 62, 190]),  // blue
        (0.35, [30, 180, 150]), // teal
        (0.55, [60, 210, 70]),  // green
        (0.78, [235, 220, 45]), // yellow
        (1.00, [235, 45, 40]),  // red
    ])
}

/// Look the ramp up at `t` in 0..1.
pub fn prop_ramp_at(t: f32) -> [u8; 3] {
    let lut = prop_ramp();
    let i = ((t.clamp(0.0, 1.0) * 255.0) as usize) * 4;
    [lut[i], lut[i + 1], lut[i + 2]]
}

/// A hue per band, for the all-bands propagation display.
///
/// Band is a categorical variable that happens to have an order, and running it
/// around the hue circle low-to-high is how every propagation site in the hobby
/// already draws it — so an operator reads this without being taught. A rainbow
/// would be the wrong choice for a continuous quantity; the per-band display
/// exists precisely for when the hues cannot be told apart, and the legend
/// names each band beside its swatch rather than relying on hue recall.
pub fn band_color(band: sdroxide_types::Band) -> [u8; 3] {
    use sdroxide_types::Band;
    match band {
        Band::M160 => [176, 40, 40], // deep red
        Band::M80 => [214, 92, 32],  // orange
        Band::M60 => [222, 150, 40], // amber
        Band::M40 => [226, 208, 52], // yellow
        Band::M30 => [150, 214, 60], // yellow-green
        Band::M20 => [58, 200, 96],  // green
        Band::M17 => [46, 200, 170], // teal
        Band::M15 => [52, 168, 226], // sky
        Band::M12 => [70, 116, 232], // blue
        Band::M10 => [122, 92, 236], // indigo
        Band::M6 => [176, 84, 226],  // violet
        Band::M4 => [202, 78, 224],  // purple
        Band::M2 => [226, 76, 190],  // magenta
        Band::M70 => [236, 96, 140], // rose
        // Not a band: nothing is ever binned here.
        Band::Gen => [128, 128, 128],
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;

    /// The ramp has to actually run blue → green → yellow → red, because that
    /// order is the whole of what it communicates.
    #[test]
    fn the_propagation_ramp_runs_cold_to_hot() {
        let cold = prop_ramp_at(0.0);
        let warm = prop_ramp_at(0.6);
        let hot = prop_ramp_at(1.0);
        assert!(cold[2] > cold[0], "the cold end is not blue: {cold:?}");
        assert!(warm[1] > warm[0] && warm[1] > warm[2], "the middle is not green: {warm:?}");
        assert!(hot[0] > hot[1] && hot[0] > hot[2], "the hot end is not red: {hot:?}");
    }

    /// Every band gets its own hue, or two bands on one cell would be
    /// indistinguishable in the combined display.
    #[test]
    fn no_two_bands_share_a_colour() {
        use sdroxide_types::Band;
        let mut seen: Vec<[u8; 3]> = Vec::new();
        for b in Band::ALL {
            if b == Band::Gen {
                continue;
            }
            let c = band_color(b);
            assert!(!seen.contains(&c), "{} reuses {c:?}", b.label());
            seen.push(c);
        }
    }
}
