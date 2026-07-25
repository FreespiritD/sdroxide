//! Turns the ephemeris plus the user's settings into a flat list of GPU draws.
//!
//! Deliberately free of wgpu types: everything here is plain `Pod` data, so the
//! geometry can be reasoned about (and unit-tested) without a GPU.

use eframe::egui::Color32;
use sdroxide_solar::{SolarData, ephem};

use super::camera::Camera;
use super::math::{M4, V3, v3};
use super::state::{Focus, SolarUi};
use crate::theme;
use crate::view::solar_layer as layer;

/// Per-frame scene constants. 160 bytes; keep in step with `Globals` in the
/// shaders.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Globals {
    pub view_proj: [[f32; 4]; 4],
    /// xyz = eye position, w = near plane.
    pub camera_pos: [f32; 4],
    /// xyz = Sun centre, w = its rendered radius.
    pub sun_pos: [f32; 4],
    /// xyz = unit Sun→Earth, w = the SDO disk radius as a fraction of the image.
    pub sun_to_earth: [f32; 4],
    /// xyz = unit solar north, w = the Stonyhurst west sign.
    pub solar_north: [f32; 4],
    /// Viewport in pixels: w, h, 1/w, 1/h.
    pub viewport: [f32; 4],
    /// x = seconds (animation phase), y = photo/procedural blend for the Sun,
    /// z, w = spare.
    pub misc: [f32; 4],
}

/// Per-draw constants. Also 160 bytes, uploaded at a dynamic offset.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DrawData {
    pub model: [[f32; 4]; 4],
    /// Rotation only (no scale), so normals and body-space lookups stay unit.
    /// A `mat4x4` rather than `mat3x3` on purpose: WGSL pads `mat3x3` columns
    /// to 16 bytes, which silently corrupts a naively packed uniform.
    pub basis: [[f32; 4]; 4],
    pub tint: [f32; 4],
    /// x = shading mode, y = cone half-angle (radians), z = alpha, w = the
    /// cone's inner radius as a fraction of its length.
    pub params: [f32; 4],
}

/// Shading branch selected by `DrawData::params.x`.
pub const MODE_EARTH: f32 = 0.0;
pub const MODE_MOON: f32 = 1.0;
pub const MODE_SUN: f32 = 2.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineInst {
    pub a: [f32; 3],
    pub width_px: f32,
    pub b: [f32; 3],
    pub _pad: f32,
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpriteInst {
    pub center: [f32; 3],
    pub size_px: f32,
    pub color: [f32; 4],
    /// x = kind (see `SPRITE_*`), y..w spare.
    pub params: [f32; 4],
}

pub const SPRITE_GLOW: f32 = 0.0;
pub const SPRITE_STAR: f32 = 1.0;
pub const SPRITE_RING: f32 = 2.0;
pub const SPRITE_DOT: f32 = 3.0;

/// Which static mesh a draw uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prim {
    Sphere,
    Cone,
}

#[derive(Default)]
pub struct Scene {
    pub globals: Globals,
    pub draws: Vec<(Prim, DrawData)>,
    pub lines: Vec<LineInst>,
    pub sprites: Vec<SpriteInst>,
    /// The star field is a static buffer on the GPU, so it is a flag here
    /// rather than 1500 instances rebuilt every frame.
    pub draw_stars: bool,
}

impl Default for Globals {
    fn default() -> Self {
        Globals {
            view_proj: M4::IDENTITY.cols,
            camera_pos: [0.0; 4],
            sun_pos: [0.0; 4],
            sun_to_earth: [1.0, 0.0, 0.0, 0.45],
            solar_north: [0.0, 0.0, 1.0, 1.0],
            viewport: [1.0, 1.0, 1.0, 1.0],
            misc: [0.0; 4],
        }
    }
}

/// sRGB → linear. The offscreen target is an sRGB format, so the hardware
/// encodes on write; shader constants therefore have to be linear or every
/// colour comes out washed out relative to the rest of the UI.
pub fn lin(c: Color32, alpha: f32) -> [f32; 4] {
    let f = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.040_45 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    [f(c.r()), f(c.g()), f(c.b()), alpha]
}

/// Positions of everything, in the heliocentric ecliptic frame, gigametres.
pub struct Bodies {
    pub jd: f64,
    pub sun_r: f32,
    pub earth: V3,
    pub earth_r: f32,
    pub earth_basis: (V3, V3, V3),
    pub moon: V3,
    pub moon_r: f32,
    pub sun_frame: ephem::SunFrame,
}

pub fn bodies(st: &SolarUi, unix_s: f64) -> Bodies {
    let jd = ephem::julian_day(unix_s);
    let v = &st.view;
    let earth = V3::from_f64(ephem::earth_heliocentric(jd));
    let b = ephem::earth_basis(jd);
    let moon_off = V3::from_f64(ephem::moon_geocentric_vec(jd)) * v.moon_orbit_scale;
    Bodies {
        jd,
        sun_r: ephem::SUN_R as f32 * v.sun_scale,
        earth,
        earth_r: ephem::EARTH_R as f32 * v.body_scale,
        earth_basis: (V3::from_f64(b.x), V3::from_f64(b.y), V3::from_f64(b.z)),
        moon: earth + moon_off,
        moon_r: ephem::MOON_R as f32 * v.body_scale,
        sun_frame: ephem::sun_frame(jd),
    }
}

impl Bodies {
    /// Where the camera pivots, and a characteristic radius used to clamp how
    /// close it may get.
    pub fn focus(&self, f: Focus) -> (V3, f32) {
        match f {
            Focus::Sun => (V3::ZERO, self.sun_r),
            Focus::Earth => (self.earth, self.earth_r),
            Focus::Moon => (self.moon, self.moon_r),
            Focus::EarthMoon => (
                (self.earth + self.moon) * 0.5,
                ((self.moon - self.earth).len() * 0.5).max(self.earth_r),
            ),
        }
    }
}

/// Build the frame's draw lists.
pub fn build(
    st: &SolarUi,
    data: Option<&SolarData>,
    unix_s: f64,
    size_px: [f32; 2],
    anim_t: f32,
) -> Scene {
    let b = bodies(st, unix_s);
    let cam = Camera::from_view(st, &b, size_px);
    let sun_img = data.and_then(|d| d.sun.as_ref());
    let mut s = Scene {
        globals: Globals {
            view_proj: cam.view_proj.cols,
            camera_pos: cam.eye.arr4(cam.near),
            sun_pos: V3::ZERO.arr4(b.sun_r),
            sun_to_earth: V3::from_f64(b.sun_frame.to_earth)
                .arr4(sun_img.map_or(0.45, |i| i.disk_radius_frac)),
            solar_north: V3::from_f64(b.sun_frame.basis.z).arr4(1.0),
            viewport: [size_px[0], size_px[1], 1.0 / size_px[0], 1.0 / size_px[1]],
            // misc.y blends the photograph in; zero until one has arrived, so
            // the procedural surface is what shows offline.
            misc: [anim_t, if sun_img.is_some() { 1.0 } else { 0.0 }, 0.0, 0.0],
        },
        draw_stars: st.layer(layer::STARS),
        ..Default::default()
    };

    bodies_draws(&mut s, st, &b, &cam);
    if st.layer(layer::ORBITS) {
        orbits(&mut s, st, &b);
    }
    if st.layer(layer::GRID) {
        grid(&mut s, &b);
    }
    markers(&mut s, st, &b, &cam);
    if st.layer(layer::QSO) {
        digi_traffic(&mut s, st, &b, &cam, anim_t);
    }
    if let Some(d) = data {
        let now = unix_s as i64;
        if st.layer(layer::SPOTS) {
            spots(&mut s, &b, &cam, &d.regions, now);
        }
        if st.layer(layer::FLARES) {
            flares(&mut s, &b, &cam, &d.flares, now);
        }
        if st.layer(layer::CME) {
            cones(&mut s, st, &d.cmes, now);
        }
    }
    s
}

/// Where a sunspot region is *now*, in Stonyhurst longitude.
///
/// Uses the Carrington longitude rather than propagating the observed
/// Stonyhurst one: Carrington longitude is fixed to the rotating Sun, so
/// `L_stonyhurst = L_carrington − L0(now)` is exact for any age of report and
/// needs no differential-rotation model. (The residual drift from differential
/// rotation is a degree or two over the life of a region.)
fn region_longitude_now(region: &sdroxide_solar::ActiveRegion, jd: f64) -> f64 {
    let (_, _, l0) = ephem::solar_p_b0_l0(jd);
    ephem::wrap180(region.carrington_deg - l0)
}

/// Sunspot active regions, from the NOAA daily summary.
///
/// Drawn on the photosphere so the depth buffer hides the far-side ones for
/// free — a marker behind the opaque Sun simply fails the depth test.
fn spots(
    s: &mut Scene,
    b: &Bodies,
    cam: &Camera,
    regions: &[sdroxide_solar::ActiveRegion],
    _now: i64,
) {
    let sun_px = cam.pixels_for(V3::ZERO, b.sun_r);
    // Below this the disk is too small to place anything on meaningfully.
    let fade = ((sun_px - 12.0) / 30.0).clamp(0.0, 1.0);
    if fade <= 0.0 {
        return;
    }
    for r in regions {
        let lon = region_longitude_now(r, b.jd);
        let dir = V3::from_f64(b.sun_frame.direction(r.lat_deg, lon));
        // Just clear of the surface, so it is not z-fighting the sphere.
        let pos = dir * (b.sun_r * 1.004);
        let radius_px = cam.pixels_for(pos, b.sun_r * r.angular_radius() as f32);
        // Colour by NOAA's own flare probability: the regions worth watching
        // are the ones likely to produce something.
        let t = r.threat() as f32;
        let color = if t > 0.5 {
            theme::PINK
        } else if t > 0.2 {
            theme::YELLOW
        } else {
            Color32::from_rgb(0x30, 0x20, 0x14)
        };
        s.sprites.push(SpriteInst {
            center: pos.arr(),
            size_px: (radius_px * 2.0).clamp(3.5, 60.0),
            color: lin(color, (0.55 + 0.45 * t) * fade),
            params: [SPRITE_DOT, 0.0, 0.0, 0.0],
        });
    }
}

/// Recent flares, marked at their source region and fading out over a day.
fn flares(
    s: &mut Scene,
    b: &Bodies,
    cam: &Camera,
    events: &[sdroxide_solar::FlareEvent],
    now: i64,
) {
    const FADE_S: f64 = 86_400.0;
    let sun_px = cam.pixels_for(V3::ZERO, b.sun_r);
    if sun_px < 12.0 {
        return;
    }
    for f in events {
        let Some((lat, lon0)) = f.location else { continue };
        let age = (now - f.peak_unix) as f64;
        if !(0.0..FADE_S).contains(&age) {
            continue;
        }
        // The region has rotated since the flare; carry it forward at the
        // synodic rate for its latitude.
        let days = age / 86_400.0;
        let lon = lon0
            + (ephem::sidereal_rotation_deg_per_day(lat) - ephem::EARTH_MEAN_MOTION_DEG_PER_DAY)
                * days;
        let dir = V3::from_f64(b.sun_frame.direction(lat, ephem::wrap180(lon)));
        let alpha = (1.0 - age / FADE_S) as f32;
        let sev = f.severity() as f32;
        s.sprites.push(SpriteInst {
            center: (dir * (b.sun_r * 1.01)).arr(),
            size_px: 8.0 + 7.0 * (sev - 2.0).clamp(0.0, 2.5),
            color: lin(theme::PINK, (0.35 + 0.55 * alpha).min(1.0)),
            params: [SPRITE_RING, 0.0, 0.0, 0.0],
        });
    }
}

/// CME trajectory cones.
///
/// The apex sits at the Sun and the leading edge at `speed × elapsed`, so the
/// picture is a direct read-out of where the plasma actually is — and whether
/// the Earth is inside the cone.
fn cones(s: &mut Scene, st: &SolarUi, cmes: &[sdroxide_solar::CmeEvent], now: i64) {
    let window = (st.view.cme_window_h as f64 * 3600.0) as i64;
    for e in cmes {
        let Some(a) = &e.analysis else { continue };
        let age = now - a.t21_5_unix;
        if !(0..window).contains(&age) {
            continue;
        }
        // The front never precedes the launch height, and a week-old event is
        // stopped before it stretches out past the outer planets.
        let launch = sdroxide_solar::impact::LAUNCH_RADIUS;
        let length = sdroxide_solar::impact::front_distance(a, now)
            .clamp(launch * 1.08, 1.8 * sdroxide_solar::AU);
        let axis = V3::from_f64(sdroxide_solar::impact::axis(a));
        let (x, y) = perpendicular_basis(axis);

        // Speed, over the range CMEs actually occupy: the slow majority sit
        // near 400 km/s and anything past ~1200 is a fast, geoeffective event.
        // A wider ramp would render almost every cone the same colour.
        let fast = ((a.speed_km_s - 350.0) / 850.0).clamp(0.0, 1.0) as f32;
        let earth_bound = sdroxide_solar::earth_impact(a).is_some();
        let color = if earth_bound {
            // The one that matters gets the alarm colour, whatever its speed.
            theme::PINK
        } else {
            Color32::from_rgb(
                (fast * 255.0) as u8,
                (0xd0 as f32 - fast * 0x50 as f32) as u8,
                (0xf4 as f32 - fast * 0x74 as f32) as u8,
            )
        };
        // Fade with age so the display does not silt up with old events.
        let mut alpha = (1.0 - age as f32 / window as f32).clamp(0.15, 1.0) * 0.5;
        // An estimated axis (from `sourceLocation` rather than a fit) is a
        // coarser number, and is drawn fainter to say so.
        if a.estimated {
            alpha *= 0.55;
        }
        if earth_bound {
            alpha = (alpha * 1.6).min(0.95);
        }

        s.draws.push((
            Prim::Cone,
            DrawData {
                model: M4::from_basis(x, y, axis, V3::ZERO, length as f32).cols,
                basis: M4::from_basis(x, y, axis, V3::ZERO, 1.0).cols,
                tint: lin(color, 1.0),
                params: [
                    0.0,
                    (a.half_angle_deg as f32).to_radians(),
                    alpha,
                    (launch / length) as f32,
                ],
            },
        ));
    }
}

/// Any orthonormal pair perpendicular to `z`. The choice is arbitrary — the
/// cone is rotationally symmetric about its axis — but it must be numerically
/// stable, hence picking the seed axis `z` is least aligned with.
fn perpendicular_basis(z: V3) -> (V3, V3) {
    let seed = if z.z.abs() < 0.9 { v3(0.0, 0.0, 1.0) } else { v3(1.0, 0.0, 0.0) };
    let x = seed.cross(z).normalize();
    (x, z.cross(x))
}

fn bodies_draws(s: &mut Scene, st: &SolarUi, b: &Bodies, cam: &Camera) {
    let ident = M4::IDENTITY;

    // Sun.
    s.draws.push((
        Prim::Sphere,
        DrawData {
            model: M4::from_basis(
                V3::from_f64(b.sun_frame.basis.x),
                V3::from_f64(b.sun_frame.basis.y),
                V3::from_f64(b.sun_frame.basis.z),
                V3::ZERO,
                b.sun_r,
            )
            .cols,
            basis: M4::from_basis(
                V3::from_f64(b.sun_frame.basis.x),
                V3::from_f64(b.sun_frame.basis.y),
                V3::from_f64(b.sun_frame.basis.z),
                V3::ZERO,
                1.0,
            )
            .cols,
            tint: lin(Color32::from_rgb(0xff, 0xc4, 0x6a), 1.0),
            params: [MODE_SUN, 0.0, 1.0, 0.0],
        },
    ));

    // Earth — its body frame is ECEF, so the land mask, the QTH marker and the
    // terminator all share one coordinate system.
    let (ex, ey, ez) = b.earth_basis;
    s.draws.push((
        Prim::Sphere,
        DrawData {
            model: M4::from_basis(ex, ey, ez, b.earth, b.earth_r).cols,
            basis: M4::from_basis(ex, ey, ez, V3::ZERO, 1.0).cols,
            tint: lin(theme::CYAN, 1.0),
            params: [MODE_EARTH, 0.0, 1.0, 0.0],
        },
    ));

    // Moon.
    s.draws.push((
        Prim::Sphere,
        DrawData {
            model: M4::from_translation_scale(b.moon, b.moon_r).cols,
            basis: ident.cols,
            tint: lin(Color32::from_rgb(0x9a, 0xa4, 0xb4), 1.0),
            params: [MODE_MOON, 0.0, 1.0, 0.0],
        },
    ));

    // A glow billboard with a pixel floor under every body, so "can I see the
    // Earth from 2 AU" never depends on the exaggeration slider.
    for (pos, radius, min_px, color) in [
        (V3::ZERO, b.sun_r, 22.0, Color32::from_rgb(0xff, 0xd0, 0x80)),
        (b.earth, b.earth_r, 7.0, theme::CYAN),
        (b.moon, b.moon_r, 5.0, Color32::from_rgb(0xc8, 0xd2, 0xe0)),
    ] {
        let px = cam.pixels_for(pos, radius);
        s.sprites.push(SpriteInst {
            center: pos.arr(),
            size_px: (px * 2.6).max(min_px),
            color: lin(color, if px > min_px * 0.5 { 0.35 } else { 0.9 }),
            params: [SPRITE_GLOW, 0.0, 0.0, 0.0],
        });
    }
    let _ = st;
}

/// Orbital paths, sampled from the same ephemeris that places the bodies — so
/// the ring is the real (eccentric) orbit rather than an idealised circle.
fn orbits(s: &mut Scene, st: &SolarUi, b: &Bodies) {
    const EARTH_STEPS: usize = 256;
    let mut prev = V3::from_f64(ephem::earth_heliocentric(b.jd));
    for k in 1..=EARTH_STEPS {
        let jd = b.jd + 365.256_363 * k as f64 / EARTH_STEPS as f64;
        let p = V3::from_f64(ephem::earth_heliocentric(jd));
        s.lines.push(seg(prev, p, 1.6, lin(theme::CYAN_DIM, 0.55)));
        prev = p;
    }

    // The Moon's path is drawn around the Earth's *current* position, so it
    // reads as a ring on the Earth rather than a smear along the Earth's orbit.
    const MOON_STEPS: usize = 128;
    let moon_at = |jd: f64| {
        b.earth + V3::from_f64(ephem::moon_geocentric_vec(jd)) * st.view.moon_orbit_scale
    };
    let mut prev = moon_at(b.jd);
    for k in 1..=MOON_STEPS {
        let jd = b.jd + 27.321_661 * k as f64 / MOON_STEPS as f64;
        let p = moon_at(jd);
        s.lines.push(seg(prev, p, 1.3, lin(theme::LINE_LIT, 0.7)));
        prev = p;
    }
}

/// Solar rotation axis and equator, and the ecliptic plane reference ring.
fn grid(s: &mut Scene, b: &Bodies) {
    let n = V3::from_f64(b.sun_frame.basis.z);
    s.lines.push(seg(n * (-b.sun_r * 1.6), n * (b.sun_r * 1.6), 1.4, lin(theme::YELLOW, 0.55)));

    const RING: usize = 96;
    let mut prev = V3::from_f64(b.sun_frame.direction(0.0, 0.0)) * (b.sun_r * 1.004);
    for k in 1..=RING {
        let lon = 360.0 * k as f64 / RING as f64;
        let p = V3::from_f64(b.sun_frame.direction(0.0, lon)) * (b.sun_r * 1.004);
        s.lines.push(seg(prev, p, 1.2, lin(theme::YELLOW, 0.4)));
        prev = p;
    }

    // Heliographic parallels every 30°, so latitude is readable on the disk.
    for lat in [-60.0, -30.0, 30.0, 60.0] {
        let mut prev = V3::from_f64(b.sun_frame.direction(lat, 0.0)) * (b.sun_r * 1.004);
        for k in 1..=RING / 2 {
            let lon = 360.0 * k as f64 / (RING / 2) as f64;
            let p = V3::from_f64(b.sun_frame.direction(lat, lon)) * (b.sun_r * 1.004);
            s.lines.push(seg(prev, p, 1.0, lin(theme::YELLOW, 0.22)));
            prev = p;
        }
    }
}

/// The operator's QTH and the sub-solar point.
///
/// Both are *positions on the Earth's surface*, so they only mean anything once
/// the Earth is big enough on screen to place them on. Below that they are
/// faded out entirely — a 13 px ring around a 2 px Earth reads as a property of
/// the planet rather than of a location on it.
fn markers(s: &mut Scene, st: &SolarUi, b: &Bodies, cam: &Camera) {
    let earth_px = cam.pixels_for(b.earth, b.earth_r);
    let fade = ((earth_px - 3.0) / 9.0).clamp(0.0, 1.0);
    if fade <= 0.0 {
        return;
    }
    // Never wider than the planet it sits on.
    let size = |base: f32| base.min(earth_px * 1.5);

    let (ex, ey, ez) = b.earth_basis;
    let on_earth = |lat: f64, lon: f64, lift: f32| {
        let d = ephem::geodetic_to_body(lat, lon);
        b.earth + (ex * d.x as f32 + ey * d.y as f32 + ez * d.z as f32) * (b.earth_r * lift)
    };

    if let Some((lat, lon)) = st.qth {
        s.sprites.push(SpriteInst {
            center: on_earth(lat, lon, 1.02).arr(),
            size_px: size(14.0),
            color: lin(theme::GREEN, 0.95 * fade),
            params: [SPRITE_RING, 0.0, 0.0, 0.0],
        });
    }

    let (slat, slon) = ephem::subsolar_point(b.jd);
    s.sprites.push(SpriteInst {
        center: on_earth(slat, slon, 1.02).arr(),
        size_px: size(9.0),
        color: lin(theme::YELLOW, 0.85 * fade),
        params: [SPRITE_DOT, 0.0, 0.0, 0.0],
    });
}

fn seg(a: V3, b: V3, width_px: f32, color: [f32; 4]) -> LineInst {
    LineInst { a: a.arr(), width_px, b: b.arr(), _pad: 0.0, color }
}

/// Decoded FT8/FT4 stations, and the path to the one being worked.
///
/// The flat map in the FT8 panel draws the same information as a great-circle
/// line across a rectangle; here the path is the *actual* great circle, lifted
/// off the surface so it arcs through space between the two stations instead of
/// disappearing round the back of the globe.
fn digi_traffic(s: &mut Scene, st: &SolarUi, b: &Bodies, cam: &Camera, anim_t: f32) {
    let earth_px = cam.pixels_for(b.earth, b.earth_r);
    // Below this the Earth is too small for a point on its surface to mean
    // anything, same threshold the QTH marker uses.
    let fade = ((earth_px - 3.0) / 9.0).clamp(0.0, 1.0);
    if fade <= 0.0 {
        return;
    }
    let (ex, ey, ez) = b.earth_basis;
    let to_world = |v: sdroxide_solar::Vec3, lift: f32| {
        b.earth + (ex * v.x as f32 + ey * v.y as f32 + ez * v.z as f32) * (b.earth_r * lift)
    };

    for (lat, lon, age) in &st.digi.stations {
        if *age <= 0.0 {
            continue;
        }
        s.sprites.push(SpriteInst {
            center: to_world(ephem::geodetic_to_body(*lat, *lon), 1.015).arr(),
            size_px: (4.0 + 3.0 * age).min(earth_px * 0.9),
            color: lin(theme::TEXT_STRONG, 0.85 * age * fade),
            params: [SPRITE_DOT, 0.0, 0.0, 0.0],
        });
    }

    // The arcs need a home to start from.
    let Some(home) = st.qth else { return };
    for (target, color, width, animated) in [
        (st.digi.dx, theme::CYAN, 2.4, true),
        (st.digi.preview, theme::YELLOW, 1.6, false),
    ] {
        let Some(dx) = target else { continue };
        arc(s, b, &to_world, home, dx, color, width, fade, animated.then_some(anim_t), st.digi.transmitting);
    }
}

/// A great-circle arc between two points on the globe, bowed out into space.
///
/// The lift is proportional to the angular separation, so a short contact
/// hugs the surface and an antipodal one springs well clear of it — which is
/// also the only way both ends stay visible at once on a sphere.
#[allow(clippy::too_many_arguments)]
fn arc(
    s: &mut Scene,
    b: &Bodies,
    to_world: &impl Fn(sdroxide_solar::Vec3, f32) -> V3,
    from: (f64, f64),
    to: (f64, f64),
    color: Color32,
    width_px: f32,
    fade: f32,
    anim: Option<f32>,
    transmitting: bool,
) {
    const STEPS: usize = 96;
    let a = ephem::geodetic_to_body(from.0, from.1);
    let c = ephem::geodetic_to_body(to.0, to.1);
    let omega = a.dot(c).clamp(-1.0, 1.0).acos();
    if omega < 1e-4 {
        return;
    }
    let bulge = 0.06 + 0.42 * (omega / std::f64::consts::PI) as f32;

    let point = |t: f64| {
        // Spherical interpolation, so the path is the true great circle rather
        // than a chord through the planet.
        let s0 = ((1.0 - t) * omega).sin() / omega.sin();
        let s1 = (t * omega).sin() / omega.sin();
        let dir = a * s0 + c * s1;
        let lift = 1.0 + bulge * (std::f64::consts::PI * t).sin() as f32;
        to_world(dir.normalize(), lift)
    };

    let mut prev = point(0.0);
    for k in 1..=STEPS {
        let t = k as f64 / STEPS as f64;
        let p = point(t);
        let mid = (k as f32 - 0.5) / STEPS as f32;
        // A travelling bright band along the path while transmitting, the same
        // cue the flat FT8 map uses for an outgoing transmission.
        let pulse = match anim {
            Some(t0) if transmitting => {
                let head = (t0 * 0.55).fract();
                let d = (mid - head).abs().min(1.0 - (mid - head).abs());
                1.0 + 2.6 * (-d * d * 220.0).exp()
            }
            _ => 1.0,
        };
        s.lines.push(seg(prev, p, width_px * pulse.min(1.9), lin(color, (0.55 * pulse).min(1.0) * fade)));
        prev = p;
    }

    // Anchor ticks: a short radial stub at each end, so the arc visibly lands
    // on the surface rather than floating near it.
    for (lat, lon) in [from, to] {
        let d = ephem::geodetic_to_body(lat, lon);
        s.lines.push(seg(to_world(d, 1.0), to_world(d, 1.0 + bulge * 0.16), width_px, lin(color, 0.7 * fade)));
    }
    let _ = b;
}

/// A fixed star field, generated once. Uniform on the sphere via the inverse
/// transform of the z coordinate; a plain LCG keeps it reproducible without a
/// dependency (and without `Math::random`, which is unavailable in wasm anyway).
pub fn stars() -> Vec<SpriteInst> {
    /// Far enough that the camera's motion within the solar system gives no
    /// visible parallax, but inside the far plane.
    const R: f32 = 300_000.0;
    const N: usize = 1500;
    let mut seed: u64 = 0x5150_5344_524f_5849;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as f64 / (1u64 << 31) as f64) as f32
    };
    (0..N)
        .map(|_| {
            let z = next() * 2.0 - 1.0;
            let phi = next() * std::f32::consts::TAU;
            let r = (1.0 - z * z).max(0.0).sqrt();
            let dir = v3(r * phi.cos(), r * phi.sin(), z);
            // A shallow magnitude distribution: mostly faint, a few bright.
            let m = next();
            let bright = 0.25 + 0.75 * m * m * m;
            SpriteInst {
                center: (dir * R).arr(),
                size_px: 1.1 + 2.2 * m * m,
                color: [bright * 0.8, bright * 0.86, bright, 1.0],
                params: [SPRITE_STAR, 0.0, 0.0, 0.0],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Solar3dView;

    fn ui() -> SolarUi {
        let mut st = SolarUi::new(Solar3dView::default());
        st.set_qth("JN78ve");
        st
    }

    #[test]
    fn uniform_blocks_are_the_size_the_shaders_expect() {
        assert_eq!(std::mem::size_of::<Globals>(), 160);
        assert_eq!(std::mem::size_of::<DrawData>(), 160);
        assert_eq!(std::mem::size_of::<LineInst>(), 48);
        assert_eq!(std::mem::size_of::<SpriteInst>(), 48);
    }

    #[test]
    fn bodies_sit_where_the_solar_system_puts_them() {
        let b = bodies(&ui(), 1_784_937_600.0);
        let au = ephem::AU as f32;
        assert!((b.earth.len() / au - 1.0).abs() < 0.02, "Earth at {} AU", b.earth.len() / au);
        // The Earth's orbit is very nearly in the ecliptic plane.
        assert!(b.earth.z.abs() < 1e-4, "Earth off-plane by {}", b.earth.z);
        let moon_off = (b.moon - b.earth).len();
        assert!((0.35..0.41).contains(&moon_off), "Earth–Moon {moon_off} Gm");
    }

    #[test]
    fn a_frame_produces_every_body_plus_orbits() {
        let s = build(&ui(), None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert_eq!(s.draws.len(), 3, "Sun, Earth and Moon");
        assert!(s.draws.iter().all(|(p, _)| *p == Prim::Sphere));
        // 256 Earth-orbit + 128 Moon-orbit segments, plus the grid.
        assert!(s.lines.len() > 384, "only {} line segments", s.lines.len());
        // A glow under each body, so none of them can be invisible.
        assert_eq!(s.sprites.iter().filter(|sp| sp.params[0] == SPRITE_GLOW).count(), 3);
        assert!(s.globals.view_proj[3][3].is_finite());
    }

    /// Surface markers are positions *on* the Earth, so they appear only once
    /// the Earth is big enough on screen to place them — at the default
    /// whole-system framing it is under a pixel across.
    #[test]
    fn surface_markers_fade_in_with_the_earth() {
        let ring = |s: &Scene| s.sprites.iter().filter(|sp| sp.params[0] == SPRITE_RING).count();

        let wide = build(&ui(), None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert_eq!(ring(&wide), 0, "QTH ring drawn over a sub-pixel Earth");

        let mut close = ui();
        close.view.focus = Focus::Earth.to_u8();
        close.view.dist = 1.0;
        let s = build(&close, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert_eq!(ring(&s), 1, "no QTH ring when framed on the Earth");
        assert_eq!(s.sprites.iter().filter(|sp| sp.params[0] == SPRITE_DOT).count(), 1);
        // ...and never wider than the planet they sit on.
        let earth_px = {
            let b = bodies(&close, 1_784_937_600.0);
            Camera::from_view(&close, &b, [1600.0, 900.0]).pixels_for(b.earth, b.earth_r)
        };
        for sp in s.sprites.iter().filter(|sp| sp.params[0] != SPRITE_GLOW) {
            assert!(sp.size_px <= earth_px * 1.5 + 0.01, "marker {} px on a {earth_px} px Earth", sp.size_px);
        }
    }

    #[test]
    fn layers_actually_remove_geometry() {
        let mut st = ui();
        st.view.layers = 0;
        let s = build(&st, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert!(s.lines.is_empty(), "layers off but {} lines drawn", s.lines.len());
        assert_eq!(s.draws.len(), 3, "bodies are not a layer");
    }

    #[test]
    fn the_qth_marker_lands_on_the_daylit_side_at_local_noon() {
        // 2026-07-25, when the Sun is overhead near 15.79°E (the test QTH's
        // longitude), i.e. about 11:00 UTC.
        let st = ui();
        let unix = 1_784_977_500.0; // 2026-07-25T11:05:00Z
        let b = bodies(&st, unix);
        let (ex, ey, ez) = b.earth_basis;
        let d = ephem::geodetic_to_body(48.19, 15.79);
        let qth_normal = ex * d.x as f32 + ey * d.y as f32 + ez * d.z as f32;
        let to_sun = (V3::ZERO - b.earth).normalize();
        assert!(qth_normal.dot(to_sun) > 0.6, "QTH not near the sub-solar point");
    }

    fn cme(speed: f64, lat: f64, lon_west: f64, t: i64) -> sdroxide_solar::CmeEvent {
        sdroxide_solar::CmeEvent {
            id: "test".into(),
            start_unix: t,
            active_region: None,
            note: String::new(),
            link: String::new(),
            analysis: Some(sdroxide_solar::CmeAnalysis {
                t21_5_unix: t,
                lat_deg: lat,
                lon_west_deg: lon_west,
                half_angle_deg: 30.0,
                speed_km_s: speed,
                kind: "C".into(),
                estimated: false,
            }),
        }
    }

    /// Cones are frusta beginning at the 21.5 R☉ launch height, not full cones
    /// from the Sun's centre. Without that, a close-up of the solar disk sits
    /// *inside* every cone in the scene and is swamped by their inner surfaces.
    #[test]
    fn cme_cones_are_truncated_at_the_launch_radius() {
        let now = 1_784_937_600i64;
        let mut st = ui();
        st.view.focus = Focus::Sun.to_u8();
        let mut data = SolarData::default();
        // A fresh event, a two-day-old one, and a very old one.
        data.cmes = vec![
            cme(700.0, 5.0, 0.0, now),
            cme(700.0, 5.0, 40.0, now - 2 * 86_400),
            cme(400.0, -20.0, -90.0, now - 60 * 3600),
        ];
        let s = build(&st, Some(&data), now as f64, [1600.0, 900.0], 0.0);

        let cones: Vec<_> = s.draws.iter().filter(|(p, _)| *p == Prim::Cone).collect();
        assert_eq!(cones.len(), 3, "all three CMEs are inside the 72 h window");
        for (_, d) in &cones {
            let inner = d.params[3];
            assert!((0.0..1.0).contains(&inner), "inner radius {inner} out of range");
            // Scale is the length; the model matrix's first column is the
            // scaled basis vector, so its length is the cone's length.
            let len = (d.model[0][0].powi(2) + d.model[0][1].powi(2) + d.model[0][2].powi(2)).sqrt();
            let launch = sdroxide_solar::impact::LAUNCH_RADIUS as f32;
            assert!(len >= launch, "cone only {len} Gm long, inside the launch radius");
            assert!(
                (inner * len - launch).abs() < 0.01,
                "truncation at {} Gm, expected {launch}",
                inner * len
            );
            assert!(d.params[1] > 0.0 && d.params[2] > 0.0);
        }
        // The older event has travelled further.
        let lens: Vec<f32> = cones
            .iter()
            .map(|(_, d)| (d.model[0][0].powi(2) + d.model[0][1].powi(2) + d.model[0][2].powi(2)).sqrt())
            .collect();
        assert!(lens[1] > lens[0], "the two-day-old CME should be further out");
    }

    #[test]
    fn cme_events_outside_the_window_are_dropped() {
        let now = 1_784_937_600i64;
        let st = ui();
        let mut data = SolarData::default();
        data.cmes = vec![
            cme(700.0, 0.0, 0.0, now - 200 * 3600), // older than the 72 h window
            cme(700.0, 0.0, 0.0, now + 3600),       // not launched yet
        ];
        let s = build(&st, Some(&data), now as f64, [1600.0, 900.0], 0.0);
        assert_eq!(s.draws.iter().filter(|(p, _)| *p == Prim::Cone).count(), 0);
    }

    /// Framed on the Earth, with a contact on the other side of the planet.
    fn earth_view_with_traffic(dx: Option<(f64, f64)>) -> SolarUi {
        let mut st = ui();
        st.view.focus = Focus::Earth.to_u8();
        st.view.dist = 0.5;
        st.digi = super::super::state::DigiTraffic {
            stations: vec![(35.7, 139.7, 1.0), (-33.9, 151.2, 0.4), (40.7, -74.0, 0.05)],
            dx,
            preview: None,
            transmitting: false,
        };
        st
    }

    #[test]
    fn decoded_stations_become_dots_on_the_globe() {
        let st = earth_view_with_traffic(None);
        let s = build(&st, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        // Three stations plus the sub-solar dot.
        assert_eq!(s.sprites.iter().filter(|sp| sp.params[0] == SPRITE_DOT).count(), 4);
        // ...and they sit on the surface, not floating or buried.
        let b = bodies(&st, 1_784_937_600.0);
        for sp in s.sprites.iter().filter(|sp| sp.params[0] == SPRITE_DOT) {
            let r = (v3(sp.center[0], sp.center[1], sp.center[2]) - b.earth).len() / b.earth_r;
            assert!((1.0..1.05).contains(&r), "marker at {r} Earth radii");
        }
    }

    /// The arc has to leave the surface, or on a sphere the far half of it is
    /// hidden behind the planet and the contact reads as going nowhere.
    #[test]
    fn a_qso_arc_bows_out_into_space_and_lands_at_both_ends() {
        // Tokyo — nearly antipodal to the JN78ve test QTH, so the longest case.
        let st = earth_view_with_traffic(Some((35.7, 139.7)));
        let now = 1_784_937_600.0;
        let b = bodies(&st, now);
        let plain = build(&earth_view_with_traffic(None), None, now, [1600.0, 900.0], 0.0);
        let s = build(&st, None, now, [1600.0, 900.0], 0.0);

        let added = s.lines.len() - plain.lines.len();
        assert!(added >= 96, "only {added} arc segments");

        let radius = |p: [f32; 3]| (v3(p[0], p[1], p[2]) - b.earth).len() / b.earth_r;
        let arc: Vec<f32> = s.lines[plain.lines.len()..].iter().map(|l| radius(l.a)).collect();
        let peak = arc.iter().copied().fold(0.0f32, f32::max);
        assert!(peak > 1.25, "arc only reached {peak} Earth radii — it hugs the surface");
        // Both ends come back down to the ground.
        assert!(arc[0] < 1.02, "arc starts {} radii up", arc[0]);
        assert!(radius(s.lines.last().unwrap().b) < 1.05);
        // ...and never dips inside the planet, which would hide it.
        assert!(arc.iter().all(|r| *r >= 0.999), "arc passes through the Earth");
    }

    #[test]
    fn a_short_hop_arcs_less_than_a_long_one() {
        let now = 1_784_937_600.0;
        let peak = |dx| {
            let st = earth_view_with_traffic(Some(dx));
            let b = bodies(&st, now);
            let base = build(&earth_view_with_traffic(None), None, now, [1600.0, 900.0], 0.0);
            let s = build(&st, None, now, [1600.0, 900.0], 0.0);
            s.lines[base.lines.len()..]
                .iter()
                .map(|l| (v3(l.a[0], l.a[1], l.a[2]) - b.earth).len() / b.earth_r)
                .fold(0.0f32, f32::max)
        };
        // A neighbouring country versus the far side of the world.
        assert!(peak((48.0, 11.0)) < peak((35.7, 139.7)));
    }

    #[test]
    fn the_qso_layer_removes_stations_and_arcs() {
        let mut st = earth_view_with_traffic(Some((35.7, 139.7)));
        st.view.layers &= !layer::QSO;
        let s = build(&st, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        // Only the sub-solar dot is left.
        assert_eq!(s.sprites.iter().filter(|sp| sp.params[0] == SPRITE_DOT).count(), 1);
    }

    #[test]
    fn stars_are_a_fixed_reproducible_sphere() {
        let a = stars();
        let b = stars();
        assert_eq!(a.len(), 1500);
        assert!(a.iter().zip(&b).all(|(x, y)| x.center == y.center));
        for s in &a {
            let r = (s.center[0].powi(2) + s.center[1].powi(2) + s.center[2].powi(2)).sqrt();
            assert!((r - 300_000.0).abs() < 1.0, "star at radius {r}");
            assert_eq!(s.params[0], SPRITE_STAR);
        }
        // Roughly isotropic: no hemisphere should hold more than 60%.
        let up = a.iter().filter(|s| s.center[2] > 0.0).count();
        assert!((600..900).contains(&up), "{up}/1500 stars in the +Z hemisphere");
    }
}
