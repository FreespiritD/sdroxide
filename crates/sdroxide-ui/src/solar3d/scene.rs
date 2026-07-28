//! Turns the ephemeris plus the user's settings into a flat list of GPU draws.
//!
//! Deliberately free of wgpu types: everything here is plain `Pod` data, so the
//! geometry can be reasoned about (and unit-tested) without a GPU.

use eframe::egui::Color32;
use sdroxide_solar::{AuroraOval, SolarData, aurora, ephem};

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

/// Per-draw constants. 192 bytes, uploaded at a dynamic offset.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DrawData {
    pub model: [[f32; 4]; 4],
    /// Rotation only (no scale), so normals and body-space lookups stay unit.
    /// A `mat4x4` rather than `mat3x3` on purpose: WGSL pads `mat3x3` columns
    /// to 16 bytes, which silently corrupts a naively packed uniform.
    pub basis: [[f32; 4]; 4],
    pub tint: [f32; 4],
    /// Second colour: the dark half of a gas giant's bands, or a ring's outer
    /// tint. Unused by the modes that do not need one.
    pub tint2: [f32; 4],
    /// x = shading mode, y = cone half-angle (radians), z = alpha, w = the
    /// cone's inner radius as a fraction of its length.
    pub params: [f32; 4],
    /// Surface style, for the modes that share one shading branch: x = the
    /// [`STYLE_*`] selector, y..w its parameters (see `solar_body.wgsl`).
    pub style: [f32; 4],
}

impl DrawData {
    /// A draw with everything that is usually left alone already zeroed.
    fn new(model: M4, basis: M4, tint: Color32, mode: f32) -> DrawData {
        DrawData {
            model: model.cols,
            basis: basis.cols,
            tint: lin(tint, 1.0),
            tint2: [0.0; 4],
            params: [mode, 0.0, 1.0, 0.0],
            style: [0.0; 4],
        }
    }
}

/// Shading branch selected by `DrawData::params.x`.
pub const MODE_EARTH: f32 = 0.0;
pub const MODE_MOON: f32 = 1.0;
pub const MODE_SUN: f32 = 2.0;
/// Every other body: a procedural surface picked by `DrawData::style.x`.
pub const MODE_BODY: f32 = 3.0;

/// Surface styles for [`MODE_BODY`], in step with `solar_body.wgsl`.
pub const STYLE_CRATERED: f32 = 0.0;
pub const STYLE_CLOUDY: f32 = 1.0;
pub const STYLE_ICE_GIANT: f32 = 2.0;
pub const STYLE_ICY: f32 = 3.0;
pub const STYLE_VOLCANIC: f32 = 4.0;
pub const STYLE_HAZE: f32 = 5.0;
/// Not procedural at all: sample layer `style.y` of the body-map array.
pub const STYLE_MAPPED: f32 = 6.0;

/// Layers of that array, in `gpu::BODY_MAPS` order.
pub const MAP_MOON: f32 = 0.0;
pub const MAP_MARS: f32 = 1.0;
pub const MAP_JUPITER: f32 = 2.0;
pub const MAP_SATURN: f32 = 3.0;

/// How one body is painted: which procedural surface, in what colours, and the
/// two per-body switches that surface understands.
#[derive(Clone, Copy)]
pub struct Look {
    style: f32,
    /// Main colour — also what the body's glow and label take.
    base: Color32,
    /// The shade it varies towards: a gas giant's belts, a rocky body's basins.
    second: Color32,
    /// Style-specific: how strongly the second colour shows for a procedural
    /// surface, or which layer of the body-map array for [`STYLE_MAPPED`].
    detail: f32,
    /// Iapetus's dark leading hemisphere.
    two_tone: f32,
    /// How strong a limb glow a [`STYLE_MAPPED`] body's atmosphere gives it.
    /// Mars's dust is the only one so far; the giants' own limb darkening is
    /// already in the map.
    haze: f32,
}

/// The palette and surface each body type is drawn with.
///
/// Colours are eyeball-matched to what the body looks like through a telescope,
/// then muted a little towards the app's palette — a photographic Jupiter next
/// to this cyan Earth would read as a different program's window.
fn look_of(s: sdroxide_solar::Surface) -> Look {
    use sdroxide_solar::Surface as S;
    let look = |style, base: u32, second: u32, detail| Look {
        style,
        base: Color32::from_rgb((base >> 16) as u8, (base >> 8) as u8, base as u8),
        second: Color32::from_rgb((second >> 16) as u8, (second >> 8) as u8, second as u8),
        detail,
        two_tone: 0.0,
        haze: 0.0,
    };
    match s {
        S::Cratered => look(STYLE_CRATERED, 0x9a938a, 0x4a4642, 1.0),
        S::Cloudy => look(STYLE_CLOUDY, 0xe6d3a8, 0xc9ac74, 0.5),
        // Mars, and only Mars: the USGS Viking mosaic. Syrtis Major and the
        // caps are what any small telescope shows, so a procedural desert of
        // noise and ellipses was always going to be compared with a photograph
        // and lose. `base` is the map's own average pushed up in value — it is
        // what the glow and the label take, and MDIM 2.1's honest salmon-grey
        // is not legible against a black sky. The haze is the dust: a real,
        // faint limb, an order thinner than the Earth's.
        S::Desert => Look { haze: 0.35, ..look(STYLE_MAPPED, 0xc4785c, 0x7d3f2c, MAP_MARS) },
        // Both giants are drawn from Cassini's maps; `planet_look` picks which
        // layer, and this is only the fallback average colour.
        S::GasBands => look(STYLE_MAPPED, 0xd8be9c, 0x9a6945, MAP_JUPITER),
        S::IceGiant => look(STYLE_ICE_GIANT, 0x76c8e0, 0x3a7fb8, 0.5),
        S::Icy => look(STYLE_ICY, 0xd9e2ea, 0x8a9db0, 1.0),
        S::Volcanic => look(STYLE_VOLCANIC, 0xe0c25a, 0xa84e30, 1.0),
        S::Haze => look(STYLE_HAZE, 0xd79c4e, 0x9c662a, 0.4),
    }
}

/// The look of a specific planet: the shared surface type, plus what is true of
/// that planet alone.
fn planet_look(p: sdroxide_solar::Planet) -> Look {
    use sdroxide_solar::Planet as P;
    let mut look = look_of(p.info().surface);
    match p {
        // The two giants are drawn from Cassini's own global maps: their belts
        // and the Great Red Spot are things a viewer knows by sight, and no
        // procedural stand-in survives being compared with the photograph.
        // `base` stays the average colour, since it is what the glow and the
        // label take.
        P::Jupiter => {
            look.style = STYLE_MAPPED;
            look.detail = MAP_JUPITER;
            look.base = Color32::from_rgb(0xd8, 0xbe, 0x9c);
        }
        P::Saturn => {
            look.style = STYLE_MAPPED;
            look.detail = MAP_SATURN;
            look.base = Color32::from_rgb(0xe8, 0xd2, 0xa4);
        }
        // Neptune is the deeper blue of the two ice giants.
        P::Neptune => {
            look.base = Color32::from_rgb(0x54, 0x84, 0xd8);
            look.second = Color32::from_rgb(0x2c, 0x50, 0xa8);
        }
        _ => {}
    }
    look
}

fn moon_look(m: &sdroxide_solar::Moon) -> Look {
    let mut look = look_of(m.surface);
    // Cassini Regio: one hemisphere of Iapetus is as dark as asphalt and the
    // other is clean ice, which is the single most recognisable thing about it.
    if m.name == "Iapetus" {
        look.two_tone = 1.0;
    }
    look
}

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

/// Which static mesh a draw uses, and with which pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prim {
    Sphere,
    Cone,
    /// The sphere mesh again, drawn as an additive auroral emission shell.
    Aurora,
    /// A flat annulus: a planet's ring system.
    Ring,
}

/// A text label anchored to a point in the scene.
///
/// Drawn by the overlay with egui rather than in the 3D pass — there is no text
/// rendering on the GPU side, and projecting a handful of points is cheaper than
/// adding one.
pub struct Label {
    pub world: [f32; 3],
    pub text: String,
    pub color: Color32,
    /// Pixels to offset from the anchor, so a label does not sit on its marker.
    pub offset: [f32; 2],
    /// What clicking the label does: open a satellite's pass table, or point
    /// the camera at a body.
    pub click: Click,
}

/// What a clickable thing in the scene does.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Click {
    #[default]
    None,
    /// Open this satellite's pass table (by catalogue number).
    Sat(u64),
    /// Make this body the camera's target.
    Focus(Focus),
}

/// A body the pointer can grab to re-target the camera.
///
/// Kept apart from [`Label`] so a planet can be clicked whether or not the
/// labels layer is on, and so the disc itself is a target rather than only the
/// text beside it.
pub struct Pick {
    pub world: [f32; 3],
    /// Screen radius of the grab area, pixels.
    pub radius_px: f32,
    pub focus: Focus,
}

#[derive(Default)]
pub struct Scene {
    pub globals: Globals,
    pub draws: Vec<(Prim, DrawData)>,
    pub lines: Vec<LineInst>,
    pub sprites: Vec<SpriteInst>,
    pub labels: Vec<Label>,
    pub picks: Vec<Pick>,
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

/// A planet, placed and oriented for this frame.
pub struct PlanetBody {
    pub planet: sdroxide_solar::Planet,
    pub pos: V3,
    /// Rendered radius, which is the true one times [`PlanetBody::exaggeration`].
    pub radius: f32,
    pub basis: (V3, V3, V3),
    /// Ring system as rendered: inner radius, outer radius, opacity.
    pub rings: Option<(f32, f32, f32)>,
    /// How much the radius was exaggerated. Its moons are scaled by the same
    /// factor — orbit radii included — so the system keeps its true shape: a
    /// moon at six planet radii is drawn at six planet radii.
    pub exaggeration: f32,
}

/// A moon of one of those planets.
pub struct MoonBody {
    /// Index into [`sdroxide_solar::planets::MOONS`], which is what
    /// [`Focus::Satellite`] stores.
    pub index: usize,
    pub info: &'static sdroxide_solar::Moon,
    pub pos: V3,
    pub radius: f32,
    /// The planet it belongs to, as an index into [`Bodies::planets`].
    pub parent: usize,
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
    pub moon_basis: (V3, V3, V3),
    pub sun_frame: ephem::SunFrame,
    pub planets: Vec<PlanetBody>,
    pub moons: Vec<MoonBody>,
}

/// The most of the Sun's rendered radius any planet may reach.
///
/// The body slider goes to 20×, at which Jupiter would be twice the Sun's size
/// and the picture would be nonsense. Capping the exaggeration per body keeps
/// the ordering everyone knows — the Sun dwarfs everything — while still
/// lifting Mercury off a single pixel.
const PLANET_MAX_SUN_FRAC: f32 = 0.35;

fn basis_cols(b: sdroxide_solar::Basis) -> (V3, V3, V3) {
    (V3::from_f64(b.x), V3::from_f64(b.y), V3::from_f64(b.z))
}

pub fn bodies(st: &SolarUi, unix_s: f64) -> Bodies {
    let jd = ephem::julian_day(unix_s);
    let v = &st.view;
    let earth = V3::from_f64(ephem::earth_heliocentric(jd));
    let b = ephem::earth_basis(jd);
    let moon_off = V3::from_f64(ephem::moon_geocentric_vec(jd)) * v.moon_orbit_scale;
    let sun_r = ephem::SUN_R as f32 * v.sun_scale;

    let mut planets = Vec::with_capacity(sdroxide_solar::Planet::ALL.len());
    let mut moons = Vec::new();
    for p in sdroxide_solar::Planet::ALL {
        let info = p.info();
        let true_r = info.radius as f32;
        let radius = (true_r * v.body_scale).clamp(true_r, sun_r * PLANET_MAX_SUN_FRAC);
        let exaggeration = radius / true_r;
        let pos = V3::from_f64(p.heliocentric(jd));
        let parent = planets.len();
        planets.push(PlanetBody {
            planet: p,
            pos,
            radius,
            basis: basis_cols(p.basis(jd)),
            rings: info
                .rings
                .map(|r| (r.inner as f32 * radius, r.outer as f32 * radius, r.opacity as f32)),
            exaggeration,
        });
        for (index, m) in sdroxide_solar::planets::MOONS.iter().enumerate() {
            if m.parent != p {
                continue;
            }
            moons.push(MoonBody {
                index,
                info: m,
                pos: pos + V3::from_f64(m.offset(jd)) * (exaggeration * v.moon_orbit_scale),
                radius: m.radius as f32 * exaggeration,
                parent,
            });
        }
    }

    Bodies {
        jd,
        sun_r,
        earth,
        earth_r: ephem::EARTH_R as f32 * v.body_scale,
        earth_basis: basis_cols(b),
        moon: earth + moon_off,
        moon_r: ephem::MOON_R as f32 * v.body_scale,
        moon_basis: basis_cols(ephem::moon_basis(jd)),
        sun_frame: ephem::sun_frame(jd),
        planets,
        moons,
    }
}

impl Bodies {
    /// Unit vector, in world space, from the Earth's centre towards a point on
    /// its surface. The Earth's own orientation is in `earth_basis`, so this
    /// turns with the planet.
    pub fn surface_dir(&self, lat: f64, lon: f64) -> V3 {
        let (ex, ey, ez) = self.earth_basis;
        let v = ephem::geodetic_to_body(lat, lon);
        (ex * v.x as f32 + ey * v.y as f32 + ez * v.z as f32).normalize()
    }

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
            Focus::Planet(p) => self
                .planets
                .iter()
                .find(|b| b.planet == p)
                .map_or((V3::ZERO, self.sun_r), |b| (b.pos, b.radius)),
            Focus::Satellite(i) => self
                .moons
                .iter()
                .find(|m| m.index == i)
                // A moon can be tiny even exaggerated, so the clamp radius has
                // a floor: without one the camera may end up closer to it than
                // the near plane allows.
                .map_or((V3::ZERO, self.sun_r), |m| (m.pos, m.radius.max(1e-4))),
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
        orbits(&mut s, st, &b, &cam);
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
        if let Some(oval) = &d.aurora
            && st.layer(layer::AURORA)
        {
            aurora_shells(&mut s, &b, &cam, oval);
        }
        if st.layer(layer::SATS) {
            satellites(&mut s, st, &b, &cam, d, unix_s);
        }
    }
    // Always called: it also builds the click targets, which are not a layer —
    // turning names off should not make the planets unclickable.
    body_labels(&mut s, st, &b, &cam, size_px[1], st.layer(layer::LABELS));
    s
}

/// Amateur-radio satellites: position, orbit and label.
///
/// Distances come back from the tracker in Earth radii, so they scale with
/// whatever exaggerated radius the Earth is being drawn at — LEO stays just
/// above the surface and QO-100 stays 6.6 radii out, at any setting.
fn satellites(
    s: &mut Scene,
    st: &SolarUi,
    b: &Bodies,
    cam: &Camera,
    data: &SolarData,
    unix_s: f64,
) {
    let earth_px = cam.pixels_for(b.earth, b.earth_r);
    // Orbits are several Earth radii across, so they are legible before a point
    // on the surface is — a lower threshold than the QTH marker's.
    let fade = ((earth_px - 1.5) / 6.0).clamp(0.0, 1.0);
    if fade <= 0.0 {
        return;
    }
    let show_all = st.view.all_satellites;
    let place = |dir: sdroxide_solar::Vec3, radii: f64| {
        b.earth + V3::from_f64(dir) * (b.earth_r * radii as f32)
    };

    for sat in data.satellites().filter(|s| show_all || s.popular) {
        let Some(state) = sat.at(unix_s) else { continue };
        let pos = place(state.dir_ecliptic, state.radii);

        // Geostationary orbits read differently from low ones — one is a fixed
        // relay, the other a pass you have to catch — so they are coloured apart.
        let geo = sat.period_min > 1300.0;
        let color = if geo { theme::GREEN } else { theme::CYAN_DIM };

        s.sprites.push(SpriteInst {
            center: pos.arr(),
            size_px: if sat.popular { 7.0 } else { 4.0 },
            color: lin(color, (if sat.popular { 0.95 } else { 0.5 }) * fade),
            params: [SPRITE_DOT, 0.0, 0.0, 0.0],
        });

        // Orbit rings only for the curated set: ninety of them at once is noise.
        if sat.popular {
            let path = sat.orbit(unix_s, 96);
            for w in path.windows(2) {
                s.lines.push(seg(
                    place(w[0].0, w[0].1),
                    place(w[1].0, w[1].1),
                    1.3,
                    lin(color, 0.4 * fade),
                ));
            }
        }

        if st.layer(layer::LABELS) && sat.popular && earth_px > 24.0 {
            // The elevation is what decides whether it is workable right now,
            // so it goes in the label rather than a separate panel.
            let text = match st.qth {
                Some((lat, lon)) => {
                    let el = state.elevation_from(lat, lon);
                    if el > 0.0 {
                        format!("{}  {el:.0}°", sat.name)
                    } else {
                        format!("{}  ▼", sat.name)
                    }
                }
                None => sat.name.clone(),
            };
            s.labels.push(Label {
                world: pos.arr(),
                text,
                color: lin_color(color, 0.9 * fade),
                offset: [9.0, -7.0],
                click: Click::Sat(sat.norad_id),
            });
        }
    }
}

// ── Aurora ──────────────────────────────────────────────────────────────────
//
// The oval is drawn as a stack of concentric emission shells rather than as a
// texture painted on the globe, because that is what it is: a hundred-odd
// kilometres of thin glowing air standing above the surface. Doing it in the
// round is what produces the bright ribbon on the limb and the colour change
// with height, neither of which a flat overlay can show. See
// `shaders/solar_aurora.wgsl` for what each shell contributes.

/// The band of atmosphere the shells span, kilometres. Green oxygen emission
/// begins around 95 km; the red line is still radiating at 400.
const AURORA_BOTTOM_KM: f32 = 92.0;
const AURORA_TOP_KM: f32 = 400.0;
/// Earth mean radius, kilometres. Altitudes become fractions of whatever radius
/// the globe is *drawn* at, so the aurora stays attached to the surface at any
/// setting of the exaggeration slider — the same trick the satellites use.
const AURORA_EARTH_R_KM: f32 = 6371.0;
/// Shells are packed towards the bottom of the band, where the green emission
/// that dominates the picture lives; spacing them evenly would spend most of
/// them on the faint red top.
const AURORA_SHELL_BIAS: f32 = 1.5;

/// Altitude of shell `k` of `n`, kilometres.
fn shell_altitude_km(k: usize, n: usize) -> f32 {
    let t = if n <= 1 { 0.0 } else { k as f32 / (n - 1) as f32 };
    AURORA_BOTTOM_KM + (AURORA_TOP_KM - AURORA_BOTTOM_KM) * t.powf(AURORA_SHELL_BIAS)
}

/// How many shells to spend on the oval, given how big the Earth is on screen.
///
/// Every shell is a full additive sphere, so this is the cost knob: twenty
/// layers is worth it when the globe fills the window and pure waste when it is
/// twenty pixels across and the layering cannot be resolved at all.
pub fn shell_count(earth_px: f32) -> usize {
    match earth_px {
        px if px >= 260.0 => 20,
        px if px >= 90.0 => 14,
        px if px >= 26.0 => 8,
        _ => 4,
    }
}

/// The auroral oval: emission shells, and the contour of its equatorward edge.
fn aurora_shells(s: &mut Scene, b: &Bodies, cam: &Camera, oval: &AuroraOval) {
    let earth_px = cam.pixels_for(b.earth, b.earth_r);
    // The oval is a feature *of the surface*, so it fades in with the same
    // threshold as the QTH marker: a polar band on a three-pixel Earth reads as
    // a property of the planet rather than of a place on it.
    let fade = ((earth_px - 3.0) / 12.0).clamp(0.0, 1.0);
    if fade <= 0.0 || oval.is_empty() {
        return;
    }

    let (ex, ey, ez) = b.earth_basis;
    let n = shell_count(earth_px);
    for k in 0..n {
        let alt = shell_altitude_km(k, n);
        // The slab of atmosphere this shell stands for: half way to each
        // neighbour. Passing it to the shader is what keeps total brightness
        // independent of how many shells were drawn.
        let lo = if k == 0 { alt } else { shell_altitude_km(k - 1, n) };
        let hi = if k + 1 == n { alt } else { shell_altitude_km(k + 1, n) };
        let slab = ((hi - lo) * 0.5).max(1.0);
        let radius = 1.0 + alt / AURORA_EARTH_R_KM;
        s.draws.push((
            Prim::Aurora,
            DrawData {
                model: M4::from_basis(ex, ey, ez, b.earth, b.earth_r * radius).cols,
                basis: M4::from_basis(ex, ey, ez, V3::ZERO, 1.0).cols,
                tint: [1.0; 4],
                tint2: [0.0; 4],
                params: [alt, slab, fade, 0.0],
                style: [0.0; 4],
            },
        ));
    }

    // The contour only means anything once a line on the surface can be told
    // apart from the surface, which is a good deal closer in than the glow.
    if earth_px > 60.0 {
        aurora_edge(s, b, oval, fade);
    }
}

/// The equatorward edge of the oval, as a ring on the globe.
///
/// This is the line to compare your own latitude against, and it comes from the
/// grid rather than from a rule of thumb about Kp — so it bulges towards the
/// equator on the night side and over the magnetic poles, which is where it
/// really does.
fn aurora_edge(s: &mut Scene, b: &Bodies, oval: &AuroraOval, fade: f32) {
    /// Sample spacing along the contour, degrees of longitude.
    const STEP_DEG: usize = 4;
    /// A step larger than this is the contour ending rather than a boundary
    /// that steep; joining across it would draw a chord through the oval.
    const MAX_JUMP_DEG: f64 = 14.0;

    let (ex, ey, ez) = b.earth_basis;
    let on_earth = |lat: f64, lon: f64| {
        let d = ephem::geodetic_to_body(lat, lon);
        b.earth + (ex * d.x as f32 + ey * d.y as f32 + ez * d.z as f32) * (b.earth_r * 1.004)
    };

    for north in [true, false] {
        let profile = oval.edge_profile(north, aurora::EDGE_PCT, STEP_DEG);
        for k in 0..profile.len() {
            let j = (k + 1) % profile.len();
            let (Some(from), Some(to)) = (profile[k], profile[j]) else { continue };
            if (from - to).abs() > MAX_JUMP_DEG {
                continue;
            }
            let lon = |i: usize| -180.0 + (i * STEP_DEG) as f64;
            s.lines.push(seg(
                on_earth(from, lon(k)),
                on_earth(to, lon(j)),
                1.4,
                lin(theme::GREEN, 0.45 * fade),
            ));
        }
    }
}

/// A body is named while it is small enough on screen to need naming, and the
/// name gets out of the way once it is not.
///
/// The same rule for every body, the Earth included. A name is how you find
/// something that is a fraction of a pixel across — which, from anywhere in the
/// inner system, is what Neptune is — and it is pure clutter stamped across a
/// planet you have flown right up to and can obviously identify.
fn label_visible(px: f32, view_h: f32) -> bool {
    px < view_h * 0.18
}

/// How big a planet has to be on screen before its moons are named too.
const MOON_LABEL_PX: f32 = 26.0;

/// Names for every body, and the click targets that go with them.
fn body_labels(s: &mut Scene, st: &SolarUi, b: &Bodies, cam: &Camera, view_h: f32, labels: bool) {
    let mut add = |pos: V3, radius: f32, name: &str, color: Color32, focus: Focus, show: bool| {
        let px = cam.pixels_for(pos, radius);
        // Everything named is also clickable, and the grab area never shrinks
        // below something a pointer can actually hit.
        s.picks.push(Pick { world: pos.arr(), radius_px: px.clamp(7.0, 400.0), focus });
        if !show || !labels || !label_visible(px, view_h) {
            return;
        }
        s.labels.push(Label {
            world: pos.arr(),
            text: name.to_string(),
            color: if st.focus() == focus { theme::CYAN } else { color },
            offset: [10.0, -6.0],
            click: Click::Focus(focus),
        });
    };

    // Our own Moon follows the same rule as everybody else's: it is named once
    // the Earth is big enough on screen for the two names not to sit on top of
    // one another, and from further out the Earth's label stands for the pair.
    let earth_px = cam.pixels_for(b.earth, b.earth_r);
    for (pos, radius, name, focus, show) in [
        (V3::ZERO, b.sun_r, "SUN", Focus::Sun, true),
        (b.earth, b.earth_r, "EARTH", Focus::Earth, true),
        (b.moon, b.moon_r, "MOON", Focus::Moon, earth_px > MOON_LABEL_PX),
    ] {
        add(pos, radius, name, theme::CYAN_DIM, focus, show);
    }

    if !st.layer(layer::PLANETS) {
        return;
    }
    for p in &b.planets {
        let color = planet_look(p.planet).base;
        let name = p.planet.name().to_uppercase();
        add(p.pos, p.radius, &name, color, Focus::Planet(p.planet), true);
    }
    for m in &b.moons {
        // Same rule for every other moon.
        let parent = &b.planets[m.parent];
        let show = cam.pixels_for(parent.pos, parent.radius) > MOON_LABEL_PX;
        let color = moon_look(m.info).base;
        let name = m.info.name.to_uppercase();
        add(m.pos, m.radius, &name, color, Focus::Satellite(m.index), show);
    }
}

/// Fade a colour's alpha for a label, staying in sRGB (egui's space) rather
/// than the linear space the shaders want.
fn lin_color(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (255.0 * alpha.clamp(0.0, 1.0)) as u8)
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
                tint2: [0.0; 4],
                params: [
                    0.0,
                    (a.half_angle_deg as f32).to_radians(),
                    alpha,
                    (launch / length) as f32,
                ],
                style: [0.0; 4],
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
    let sun_basis = basis_cols(b.sun_frame.basis);
    let sphere = |s: &mut Scene, (x, y, z): (V3, V3, V3), pos, r, tint, mode, style| {
        let mut d = DrawData::new(
            M4::from_basis(x, y, z, pos, r),
            M4::from_basis(x, y, z, V3::ZERO, 1.0),
            tint,
            mode,
        );
        d.style = style;
        s.draws.push((Prim::Sphere, d));
    };

    // Sun.
    let sun = Color32::from_rgb(0xff, 0xc4, 0x6a);
    sphere(s, sun_basis, V3::ZERO, b.sun_r, sun, MODE_SUN, [0.0; 4]);

    // Earth — its body frame is ECEF, so the land mask, the QTH marker and the
    // terminator all share one coordinate system.
    sphere(s, b.earth_basis, b.earth, b.earth_r, theme::CYAN, MODE_EARTH, [0.0; 4]);

    // Moon, in the tidally locked frame — which is what puts Imbrium and
    // Tranquillitatis on the near side, where they belong.
    let moon = Color32::from_rgb(0x9a, 0xa4, 0xb4);
    sphere(s, b.moon_basis, b.moon, b.moon_r, moon, MODE_MOON, [0.0, MAP_MOON, 0.0, 0.0]);

    if st.layer(layer::PLANETS) {
        for p in &b.planets {
            body_sphere(s, p.basis, p.pos, p.radius, planet_look(p.planet));
        }
        for m in &b.moons {
            body_sphere(s, moon_facing_basis(b, m), m.pos, m.radius, moon_look(m.info));
        }
        // Rings last of all the bodies: they are transparent, so they have to
        // blend over whatever is already there — and keeping them in one run
        // costs a single pipeline switch.
        for p in &b.planets {
            rings(s, p);
        }
    }

    // A glow billboard with a pixel floor under every body, so "can I see the
    // Earth from 2 AU" never depends on the exaggeration slider.
    glow(s, cam, V3::ZERO, b.sun_r, 22.0, Color32::from_rgb(0xff, 0xd0, 0x80));
    glow(s, cam, b.earth, b.earth_r, 7.0, theme::CYAN);
    glow(s, cam, b.moon, b.moon_r, 5.0, Color32::from_rgb(0xc8, 0xd2, 0xe0));
    if st.layer(layer::PLANETS) {
        for p in &b.planets {
            glow(s, cam, p.pos, p.radius, 6.0, planet_look(p.planet).base);
        }
        for m in &b.moons {
            // A moon's glow only once its planet is big enough on screen for
            // the two to be told apart; further out the planet's own glow
            // stands for the whole system.
            if cam.pixels_for(b.planets[m.parent].pos, b.planets[m.parent].radius) > 6.0 {
                glow(s, cam, m.pos, m.radius, 3.0, moon_look(m.info).base);
            }
        }
    }
}

/// A billboard under a body, never smaller than `min_px` across.
fn glow(s: &mut Scene, cam: &Camera, pos: V3, radius: f32, min_px: f32, color: Color32) {
    let px = cam.pixels_for(pos, radius);
    s.sprites.push(SpriteInst {
        center: pos.arr(),
        size_px: (px * 2.6).max(min_px),
        color: lin(color, if px > min_px * 0.5 { 0.35 } else { 0.9 }),
        params: [SPRITE_GLOW, 0.0, 0.0, 0.0],
    });
}

/// One of the procedurally shaded bodies: a planet or one of their moons.
fn body_sphere(s: &mut Scene, (x, y, z): (V3, V3, V3), pos: V3, radius: f32, look: Look) {
    let mut d = DrawData::new(
        M4::from_basis(x, y, z, pos, radius),
        M4::from_basis(x, y, z, V3::ZERO, 1.0),
        look.base,
        MODE_BODY,
    );
    d.tint2 = lin(look.second, 1.0);
    d.style = [look.style, look.detail, look.two_tone, look.haze];
    s.draws.push((Prim::Sphere, d));
}

/// A moon's body frame. Every moon this view draws is tidally locked, so — as
/// with our own — the near side faces the planet and the frame follows from
/// the geometry rather than from a rotation rate.
fn moon_facing_basis(b: &Bodies, m: &MoonBody) -> (V3, V3, V3) {
    let parent = &b.planets[m.parent];
    let to_parent = (parent.pos - m.pos).normalize();
    let pole = parent.basis.2;
    let z = (pole - to_parent * pole.dot(to_parent)).normalize();
    (to_parent, z.cross(to_parent), z)
}

/// A planet's rings, as a flat annulus in its equatorial plane.
fn rings(s: &mut Scene, p: &PlanetBody) {
    let Some((inner, outer, opacity)) = p.rings else { return };
    let (x, y, z) = p.basis;
    let mut d = DrawData::new(
        // Scaled to the outer edge; the shader carries the inner one as a
        // fraction, so one unit annulus mesh serves every ring system.
        M4::from_basis(x, y, z, p.pos, outer),
        M4::from_basis(x, y, z, V3::ZERO, 1.0),
        Color32::from_rgb(0xe8, 0xdc, 0xc0),
        0.0,
    );
    d.tint2 = lin(Color32::from_rgb(0xa9, 0x95, 0x78), 1.0);
    // x = inner edge as a fraction of the outer, y = opacity, z = the planet's
    // radius in the same units (for the shadow it casts on them), w = which
    // radial profile: Saturn's broad sheet, or Uranus's handful of threads.
    let narrow = if opacity < 0.5 { 1.0 } else { 0.0 };
    d.params = [inner / outer, opacity, p.radius / outer, narrow];
    s.draws.push((Prim::Ring, d));
}

/// Orbital paths, sampled from the same ephemeris that places the bodies — so
/// the ring is the real (eccentric) orbit rather than an idealised circle.
fn orbits(s: &mut Scene, st: &SolarUi, b: &Bodies, cam: &Camera) {
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
    let moon_at =
        |jd: f64| b.earth + V3::from_f64(ephem::moon_geocentric_vec(jd)) * st.view.moon_orbit_scale;
    let mut prev = moon_at(b.jd);
    for k in 1..=MOON_STEPS {
        let jd = b.jd + 27.321_661 * k as f64 / MOON_STEPS as f64;
        let p = moon_at(jd);
        s.lines.push(seg(prev, p, 1.3, lin(theme::LINE_LIT, 0.7)));
        prev = p;
    }

    if !st.layer(layer::PLANETS) {
        return;
    }
    // The other planets. Sampled over one of their own years, so Neptune's
    // ring is as smooth as Mercury's rather than 165 times coarser.
    const PLANET_STEPS: usize = 192;
    for p in &b.planets {
        let period = p.planet.info().orbit_days();
        let at = |jd: f64| V3::from_f64(p.planet.heliocentric(jd));
        let mut prev = at(b.jd);
        for k in 1..=PLANET_STEPS {
            let q = at(b.jd + period * k as f64 / PLANET_STEPS as f64);
            s.lines.push(seg(prev, q, 1.2, lin(theme::CYAN_DIM, 0.3)));
            prev = q;
        }
    }

    // Moon paths, only for a planet big enough on screen for a ring around it
    // to be a ring rather than a smudge.
    const SAT_STEPS: usize = 64;
    for m in &b.moons {
        let parent = &b.planets[m.parent];
        if cam.pixels_for(parent.pos, parent.radius) < 10.0 {
            continue;
        }
        let scale = parent.exaggeration * st.view.moon_orbit_scale;
        let at = |jd: f64| parent.pos + V3::from_f64(m.info.offset(jd)) * scale;
        let mut prev = at(b.jd);
        for k in 1..=SAT_STEPS {
            let q = at(b.jd + m.info.period_d * k as f64 / SAT_STEPS as f64);
            s.lines.push(seg(prev, q, 1.0, lin(theme::LINE_LIT, 0.5)));
            prev = q;
        }
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
    for (target, color, width, animated) in
        [(st.digi.dx, theme::CYAN, 2.4, true), (st.digi.preview, theme::YELLOW, 1.6, false)]
    {
        let Some(dx) = target else { continue };
        arc(
            s,
            b,
            &to_world,
            home,
            dx,
            color,
            width,
            fade,
            animated.then_some(anim_t),
            st.digi.transmitting,
        );
    }
}

/// How far an arc spanning `omega` radians bows off the surface, as a fraction
/// of the Earth's rendered radius.
///
/// The lift is proportional to the angular separation, so a short contact hugs
/// the surface and an antipodal one springs well clear of it — which is also
/// the only way both ends stay visible at once on a sphere.
///
/// The camera's contact framing reads this too: the shot it composes has to be
/// built around the arc that actually gets drawn, not a second guess at it.
pub fn arc_bulge(omega: f64) -> f32 {
    0.06 + 0.42 * (omega / std::f64::consts::PI) as f32
}

/// A great-circle arc between two points on the globe, bowed out into space.
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
    let bulge = arc_bulge(omega);

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
        s.lines.push(seg(
            prev,
            p,
            width_px * pulse.min(1.9),
            lin(color, (0.55 * pulse).min(1.0) * fade),
        ));
        prev = p;
    }

    // Anchor ticks: a short radial stub at each end, so the arc visibly lands
    // on the surface rather than floating near it.
    for (lat, lon) in [from, to] {
        let d = ephem::geodetic_to_body(lat, lon);
        s.lines.push(seg(
            to_world(d, 1.0),
            to_world(d, 1.0 + bulge * 0.16),
            width_px,
            lin(color, 0.7 * fade),
        ));
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
        assert_eq!(std::mem::size_of::<DrawData>(), 192);
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

    /// How many bodies the scene is made of, so the counts below read as
    /// something other than magic numbers.
    fn population() -> (usize, usize) {
        (sdroxide_solar::Planet::ALL.len(), sdroxide_solar::planets::MOONS.len())
    }

    #[test]
    fn a_frame_produces_every_body_plus_orbits() {
        let (planets, moons) = population();
        let s = build(&ui(), None, 1_784_937_600.0, [1600.0, 900.0], 0.0);

        let spheres = s.draws.iter().filter(|(p, _)| *p == Prim::Sphere).count();
        assert_eq!(spheres, 3 + planets + moons, "Sun, Earth, Moon, the planets and their moons");
        // Saturn and Uranus have rings; nothing else drawn here does.
        assert_eq!(s.draws.iter().filter(|(p, _)| *p == Prim::Ring).count(), 2);
        // ...and the rings come after every sphere, so the transparent sheet
        // blends over the planet instead of being clipped by it.
        let first_ring = s.draws.iter().position(|(p, _)| *p == Prim::Ring).expect("rings");
        assert!(s.draws[..first_ring].iter().all(|(p, _)| *p == Prim::Sphere));

        // 256 Earth-orbit + 128 Moon-orbit segments, plus the grid and a ring
        // for every planet.
        assert!(s.lines.len() > 384 + 192 * planets, "only {} line segments", s.lines.len());
        // A glow under each body, so none of them can be invisible.
        assert_eq!(
            s.sprites.iter().filter(|sp| sp.params[0] == SPRITE_GLOW).count(),
            3 + planets,
            "at this framing the moons are inside their planets' glows"
        );
        assert!(s.globals.view_proj[3][3].is_finite());
    }

    /// Every planet is named from anywhere in the system — that is the only
    /// thing that makes a body a fraction of a pixel across findable — and
    /// every body is clickable whether or not it is named.
    #[test]
    fn distant_planets_are_labelled_and_clickable() {
        let (planets, moons) = population();
        let s = build(&ui(), None, 1_784_937_600.0, [1600.0, 900.0], 0.0);

        for p in sdroxide_solar::Planet::ALL {
            let name = p.name().to_uppercase();
            assert!(s.labels.iter().any(|l| l.text == name), "{name} is not labelled");
        }
        // Moons stay quiet until their planet is big enough to hang names off.
        assert!(!s.labels.iter().any(|l| l.text == "IO"), "moons labelled from 2 AU away");

        // Pick targets exist for everything, name or no name, and each is big
        // enough to actually hit.
        assert_eq!(s.picks.len(), 3 + planets + moons);
        assert!(s.picks.iter().all(|p| p.radius_px >= 7.0));
        for f in [Focus::Sun, Focus::Earth, Focus::Planet(sdroxide_solar::Planet::Neptune)] {
            assert!(s.picks.iter().any(|p| p.focus == f), "{f:?} cannot be clicked");
        }

        // With the labels layer off the names go but the targets stay.
        let mut dark = ui();
        dark.view.layers &= !layer::LABELS;
        let s = build(&dark, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert!(s.labels.is_empty());
        assert_eq!(s.picks.len(), 3 + planets + moons);
    }

    /// Framed on Jupiter, its moons get their names — and they are in the right
    /// order outward from the planet, which is the check that the table's
    /// orbit radii and the renderer's scaling agree.
    #[test]
    fn a_planets_moons_appear_when_it_fills_the_frame() {
        let mut st = ui();
        st.set_focus(Focus::Planet(sdroxide_solar::Planet::Jupiter));
        st.view.dist = 0.6;
        let s = build(&st, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        for name in ["IO", "EUROPA", "GANYMEDE", "CALLISTO"] {
            assert!(s.labels.iter().any(|l| l.text == name), "{name} missing");
        }

        let b = bodies(&st, 1_784_937_600.0);
        let jupiter =
            b.planets.iter().find(|p| p.planet == sdroxide_solar::Planet::Jupiter).unwrap();
        let radii: Vec<f32> = b
            .moons
            .iter()
            .filter(|m| m.info.parent == sdroxide_solar::Planet::Jupiter)
            .map(|m| (m.pos - jupiter.pos).len() / jupiter.radius)
            .collect();
        assert!(radii.windows(2).all(|w| w[0] < w[1]), "the Galilean order is wrong: {radii:?}");
        // Io orbits at 5.9 Jupiter radii and Callisto at 26.3, whatever the
        // exaggeration slider is set to — the system keeps its true shape.
        assert!((radii[0] - 5.9).abs() < 0.2, "Io at {} radii", radii[0]);
        assert!((radii[3] - 26.9).abs() < 0.5, "Callisto at {} radii", radii[3]);
    }

    /// The exaggeration cap: the Sun has to stay the biggest thing in the
    /// picture, or the view stops being a picture of the solar system.
    #[test]
    fn no_planet_outgrows_the_sun() {
        let mut st = ui();
        st.view.body_scale = 20.0;
        let b = bodies(&st, 1_784_937_600.0);
        for p in &b.planets {
            assert!(p.radius < b.sun_r * 0.5, "{} is {} Gm across", p.planet.name(), p.radius);
            assert!(p.radius >= p.planet.info().radius as f32, "{} shrank", p.planet.name());
            assert!(p.exaggeration >= 1.0);
        }
        // Mercury is small enough that the cap never binds: it gets the full
        // exaggeration the slider asks for.
        let mercury =
            b.planets.iter().find(|p| p.planet == sdroxide_solar::Planet::Mercury).unwrap();
        assert!((mercury.exaggeration - 20.0).abs() < 0.01);
    }

    #[test]
    fn the_planets_layer_removes_them_all() {
        let mut st = ui();
        st.view.layers &= !layer::PLANETS;
        let s = build(&st, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert_eq!(s.draws.len(), 3, "only the Sun, the Earth and the Moon are left");
        assert!(s.draws.iter().all(|(p, _)| *p == Prim::Sphere), "a ring survived");
        assert!(!s.labels.iter().any(|l| l.text == "JUPITER"));
        assert_eq!(s.picks.len(), 3);
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
            assert!(
                sp.size_px <= earth_px * 1.5 + 0.01,
                "marker {} px on a {earth_px} px Earth",
                sp.size_px
            );
        }
    }

    #[test]
    fn layers_actually_remove_geometry() {
        let mut st = ui();
        st.view.layers = 0;
        let s = build(&st, None, 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert!(s.lines.is_empty(), "layers off but {} lines drawn", s.lines.len());
        assert_eq!(s.draws.len(), 3, "the Sun, the Earth and the Moon are not a layer");
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
            let len =
                (d.model[0][0].powi(2) + d.model[0][1].powi(2) + d.model[0][2].powi(2)).sqrt();
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
            .map(|(_, d)| {
                (d.model[0][0].powi(2) + d.model[0][1].powi(2) + d.model[0][2].powi(2)).sqrt()
            })
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

    /// A synthetic oval: a solid band from 60° to 75° in both hemispheres, so
    /// the tests assert against a shape they know rather than against whatever
    /// the sky was doing when a fixture was taken.
    fn test_oval() -> AuroraOval {
        let mut grid = vec![0u8; aurora::GRID_W * aurora::GRID_H];
        for row in 0..aurora::GRID_H {
            let lat = 90.0 - row as f64;
            if (60.0..=75.0).contains(&lat.abs()) {
                for col in 0..aurora::GRID_W {
                    grid[row * aurora::GRID_W + col] = 40;
                }
            }
        }
        AuroraOval { observed_unix: 0, forecast_unix: 0, grid }
    }

    fn earth_view_with_aurora(oval: Option<AuroraOval>) -> (SolarUi, SolarData) {
        let mut st = ui();
        st.view.focus = Focus::Earth.to_u8();
        st.view.dist = 0.5;
        let mut data = SolarData::default();
        data.aurora = oval.map(std::sync::Arc::new);
        (st, data)
    }

    /// The shells have to be a *thin* shell of atmosphere standing on the
    /// surface. If they drifted to satellite altitudes the oval would detach
    /// from the globe and stop meaning anything.
    #[test]
    fn the_aurora_is_a_stack_of_shells_in_the_upper_atmosphere() {
        let (st, data) = earth_view_with_aurora(Some(test_oval()));
        let now = 1_784_937_600.0;
        let b = bodies(&st, now);
        let earth_px = Camera::from_view(&st, &b, [1600.0, 900.0]).pixels_for(b.earth, b.earth_r);
        let s = build(&st, Some(&data), now, [1600.0, 900.0], 0.0);

        let shells: Vec<_> = s.draws.iter().filter(|(p, _)| *p == Prim::Aurora).collect();
        assert_eq!(shells.len(), shell_count(earth_px), "wrong number of shells");

        let mut prev_alt = 0.0f32;
        let mut slab_total = 0.0f32;
        for (_, d) in &shells {
            let (alt, slab, intensity) = (d.params[0], d.params[1], d.params[2]);
            assert!(
                (AURORA_BOTTOM_KM..=AURORA_TOP_KM).contains(&alt),
                "shell at {alt} km is outside the emission band"
            );
            assert!(alt > prev_alt, "shells are not ordered by altitude");
            prev_alt = alt;
            assert!(slab > 0.0 && intensity > 0.0);
            slab_total += slab;

            // Scale is the first column's length, and it is the *rendered*
            // radius, so the altitude must be a fraction of the globe's own
            // radius however exaggerated that is.
            let scale =
                (d.model[0][0].powi(2) + d.model[0][1].powi(2) + d.model[0][2].powi(2)).sqrt();
            let ratio = scale / b.earth_r;
            assert!((1.014..1.064).contains(&ratio), "shell at {ratio}× the Earth's radius");
        }
        // The slabs tile the band rather than overlapping or leaving gaps.
        let band = AURORA_TOP_KM - AURORA_BOTTOM_KM;
        assert!(
            (slab_total - band).abs() < band * 0.15,
            "slabs total {slab_total} km over a {band} km band"
        );

        // Bodies first, then the shells: the draw loop only rebinds a pipeline
        // when the primitive changes, so an interleaved order would cost a
        // switch per shell.
        let first = s.draws.iter().position(|(p, _)| *p == Prim::Aurora).unwrap();
        assert!(first >= 3, "aurora drawn before the Sun, Earth and Moon");
        assert!(s.draws[first..].iter().all(|(p, _)| *p == Prim::Aurora));
    }

    #[test]
    fn fewer_shells_are_spent_on_a_smaller_earth() {
        assert!(shell_count(600.0) > shell_count(120.0));
        assert!(shell_count(120.0) > shell_count(40.0));
        assert!(shell_count(40.0) > shell_count(4.0));
        assert!(shell_count(0.0) >= 2, "an oval drawn with fewer than two shells has no depth");
    }

    /// The contour is the number an operator compares their own latitude
    /// against, so it must land on the band the data actually has.
    #[test]
    fn the_edge_contour_lands_on_the_oval() {
        let (st, data) = earth_view_with_aurora(Some(test_oval()));
        let now = 1_784_937_600.0;
        let b = bodies(&st, now);
        let (_, _, ez) = b.earth_basis;
        let (plain, _) = earth_view_with_aurora(None);
        let base = build(&plain, Some(&SolarData::default()), now, [1600.0, 900.0], 0.0);
        let s = build(&st, Some(&data), now, [1600.0, 900.0], 0.0);

        let added = &s.lines[base.lines.len()..];
        // Two rings, sampled every 4°: 90 segments each.
        assert_eq!(added.len(), 180, "expected a closed ring in each hemisphere");
        let mut north = 0;
        for l in added {
            let p = v3(l.a[0], l.a[1], l.a[2]) - b.earth;
            let r = p.len() / b.earth_r;
            assert!((1.0..1.01).contains(&r), "contour at {r} Earth radii — not on the surface");
            let lat = (p.dot(ez) / p.len()).clamp(-1.0, 1.0).asin().to_degrees();
            // The band's equatorward edge, wherever the sampler put it.
            assert!((59.0..61.0).contains(&lat.abs()), "contour at {lat}°");
            north += (lat > 0.0) as usize;
        }
        assert_eq!(north, 90, "the two hemispheres should contribute equally");
    }

    #[test]
    fn the_aurora_layer_removes_the_oval_entirely() {
        let (mut st, data) = earth_view_with_aurora(Some(test_oval()));
        st.view.layers &= !layer::AURORA;
        let s = build(&st, Some(&data), 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert!(s.draws.iter().all(|(p, _)| *p != Prim::Aurora));
    }

    /// A quiet night is not the same as no data: an all-zero grid must draw
    /// nothing at all rather than a ring of black shells over the poles.
    #[test]
    fn a_quiet_oval_draws_nothing() {
        let empty = AuroraOval {
            observed_unix: 0,
            forecast_unix: 0,
            grid: vec![0u8; aurora::GRID_W * aurora::GRID_H],
        };
        let (st, data) = earth_view_with_aurora(Some(empty));
        let s = build(&st, Some(&data), 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert!(s.draws.iter().all(|(p, _)| *p != Prim::Aurora));
    }

    /// From out by the Sun the Earth is a fraction of a pixel, and a polar band
    /// on it cannot be seen — the shells there are pure overdraw.
    #[test]
    fn the_oval_fades_out_with_a_distant_earth() {
        let mut st = ui();
        st.view.focus = Focus::Sun.to_u8();
        let mut data = SolarData::default();
        data.aurora = Some(std::sync::Arc::new(test_oval()));
        let s = build(&st, Some(&data), 1_784_937_600.0, [1600.0, 900.0], 0.0);
        assert!(s.draws.iter().all(|(p, _)| *p != Prim::Aurora));
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
