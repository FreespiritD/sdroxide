// Sun, Earth and Moon. One pipeline, one branch on `d.params.x` — the branch
// is uniform across a draw, so it costs nothing.

struct Globals {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,   // xyz eye, w near
    sun_pos: vec4<f32>,      // xyz centre, w rendered radius
    sun_to_earth: vec4<f32>, // xyz unit, w SDO disk radius as a fraction of the image
    solar_north: vec4<f32>,  // xyz unit, w Stonyhurst west sign
    viewport: vec4<f32>,     // w, h, 1/w, 1/h
    misc: vec4<f32>,         // x seconds, y photo blend, zw spare
};

struct DrawData {
    model: mat4x4<f32>,
    basis: mat4x4<f32>,
    tint: vec4<f32>,
    params: vec4<f32>,       // x mode, y half-angle, z alpha, w spare
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var land_tex: texture_2d<f32>;
@group(0) @binding(2) var sun_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(1) @binding(0) var<uniform> d: DrawData;

// The FT8 map's own palette, so the globe reads as the same map (see
// widgets/worldmap.rs, land `#1c4458`).
const LAND_DAY  = vec3<f32>(0.109804, 0.266667, 0.345098); // #1c4458
const OCEAN_DAY = vec3<f32>(0.039216, 0.094118, 0.149020); // #0a1826
const COAST     = vec3<f32>(0.113725, 0.611765, 0.745098); // #1d9cbe  theme::CYAN_DIM
const ATMO      = vec3<f32>(0.000000, 0.815686, 0.956863); // #00d0f4  theme::CYAN

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3(0.055)) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

fn hash3(p: vec3<f32>) -> f32 {
    return fract(sin(dot(p, vec3(12.9898, 78.233, 37.719))) * 43758.5453);
}

fn vnoise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let c000 = hash3(i + vec3(0.0, 0.0, 0.0));
    let c100 = hash3(i + vec3(1.0, 0.0, 0.0));
    let c010 = hash3(i + vec3(0.0, 1.0, 0.0));
    let c110 = hash3(i + vec3(1.0, 1.0, 0.0));
    let c001 = hash3(i + vec3(0.0, 0.0, 1.0));
    let c101 = hash3(i + vec3(1.0, 0.0, 1.0));
    let c011 = hash3(i + vec3(0.0, 1.0, 1.0));
    let c111 = hash3(i + vec3(1.0, 1.0, 1.0));
    let x00 = mix(c000, c100, u.x);
    let x10 = mix(c010, c110, u.x);
    let x01 = mix(c001, c101, u.x);
    let x11 = mix(c011, c111, u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

fn granulation(n: vec3<f32>) -> f32 {
    return vnoise(n * 34.0) * 0.55 + vnoise(n * 91.0) * 0.30 + vnoise(n * 210.0) * 0.15;
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) world: vec3<f32>,
};

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var o: VsOut;
    let world = d.model * vec4(pos, 1.0);
    o.clip = g.view_proj * world;
    o.world = world.xyz;
    // The mesh is a unit sphere, so its position *is* its object-space normal.
    o.nrm = normalize((d.basis * vec4(pos, 0.0)).xyz);
    o.uv = uv;
    return o;
}

fn shade_earth(in: VsOut, n: vec3<f32>) -> vec3<f32> {
    let to_sun = normalize(g.sun_pos.xyz - in.world);
    // A soft terminator: the Sun is half a degree wide and the atmosphere
    // scatters well past the geometric line.
    let day = smoothstep(-0.06, 0.16, dot(n, to_sun));

    let land = textureSample(land_tex, samp, in.uv).r;
    // Coastline from the mask's gradient — one extra tap per axis, and it
    // gives the same cyan shoreline the flat FT8 map has.
    let t = 1.5 / vec2<f32>(textureDimensions(land_tex));
    let lx = textureSample(land_tex, samp, in.uv + vec2(t.x, 0.0)).r
           - textureSample(land_tex, samp, in.uv - vec2(t.x, 0.0)).r;
    let ly = textureSample(land_tex, samp, in.uv + vec2(0.0, t.y)).r
           - textureSample(land_tex, samp, in.uv - vec2(0.0, t.y)).r;
    let coast = clamp((abs(lx) + abs(ly)) * 1.6, 0.0, 1.0);

    // The FT8 map's palette is tuned for sparse dots on a dark panel; filling a
    // whole globe with it at 1× reads as almost black, so the daylit side is
    // lifted well above the flat colour while keeping its hue.
    var col = mix(srgb_to_linear(OCEAN_DAY), srgb_to_linear(LAND_DAY), land);
    col = col * (0.05 + 2.6 * day);
    // Night side: land stays faintly visible with a cyan glow, like a city map.
    col += srgb_to_linear(COAST) * land * (1.0 - day) * 0.045;
    col = mix(col, srgb_to_linear(COAST) * (0.35 + 0.9 * day), coast * (0.35 + 0.55 * day));

    // Atmospheric limb. Brightest on the daylit edge, which is what gives the
    // globe its depth against a black background.
    let to_eye = normalize(g.camera_pos.xyz - in.world);
    let rim = pow(1.0 - clamp(dot(n, to_eye), 0.0, 1.0), 3.0);
    col += srgb_to_linear(ATMO) * rim * (0.06 + 0.55 * day);
    return col;
}

fn shade_moon(in: VsOut, n: vec3<f32>) -> vec3<f32> {
    let to_sun = normalize(g.sun_pos.xyz - in.world);
    let day = smoothstep(-0.03, 0.08, dot(n, to_sun));
    // Faint maria so the disk is not a flat ball.
    let mottle = 0.86 + 0.14 * vnoise(n * 7.0);
    return d.tint.rgb * (0.02 + 1.25 * day) * mottle;
}

fn shade_sun(in: VsOut, n: vec3<f32>) -> vec3<f32> {
    let e = g.sun_to_earth.xyz;
    // Solar north projected perpendicular to the view-from-Earth axis is "up"
    // in an SDO frame (the P angle is already removed from the browse images);
    // `rt` completes the pair, with the sign that puts heliographic west on the
    // right as seen from Earth.
    let up = normalize(g.solar_north.xyz - e * dot(g.solar_north.xyz, e));
    let rt = cross(up, e) * g.solar_north.w;
    // Facing fraction: >0 is the Earth-facing hemisphere SDO can see.
    let c = dot(n, e);

    // Limb darkening relative to the *camera*, which is what makes the sphere
    // read as a sphere from any viewpoint.
    let to_eye = normalize(g.camera_pos.xyz - in.world);
    let mu = clamp(dot(n, to_eye), 0.0, 1.0);
    let ld = 0.35 + 0.65 * pow(mu, 0.55);

    let base = d.tint.rgb * ld * (0.90 + 0.16 * granulation(n));

    // The SDO disk is an orthographic photograph of the Earth-facing side, so a
    // surface point's image coordinate is exactly its component along rt/up, in
    // solar radii — no perspective divide. It smears badly towards the limb
    // (infinite surface area compressed into no texture area), so it is faded
    // out well before the edge and the procedural surface takes over.
    let disk = g.sun_to_earth.w;
    let uv = vec2(0.5 + dot(n, rt) * disk, 0.5 - dot(n, up) * disk);
    let photo = textureSample(sun_tex, samp, uv).rgb;
    let w = g.misc.y * smoothstep(0.05, 0.35, c);
    return mix(base, photo * (0.55 + 0.45 * ld), w);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.nrm);
    let mode = d.params.x;
    var col: vec3<f32>;
    if (mode < 0.5) {
        col = shade_earth(in, n);
    } else if (mode < 1.5) {
        col = shade_moon(in, n);
    } else {
        col = shade_sun(in, n);
    }
    return vec4(col, 1.0);
}
