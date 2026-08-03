// The propagation heat map, as one shell of paint just above the surface.
//
// Unlike every other data layer in this window, this shader picks no colours.
// The whole field — the ramp, the per-band hues, the mixing where two bands
// overlap, the alpha — is resolved on the CPU in `prop_map::PropHeat::rgba` and
// arrives here as finished RGBA. The flat map in the operating panel uploads
// exactly the same bytes as an egui texture. That is deliberate: the two views
// are not merely consistent by convention, they are the same pixels, and there
// is no second copy of the colour rules to drift.
//
// What is left here is what only the GPU can do: put it on a sphere, let the
// sampler's bilinear filtering turn 2.5° cells into smooth shapes, and light it
// like paint rather than like emission.
//
// Paint, not light: the aurora adds to what is behind it because a curtain is
// glowing air you see through. Heat is a property *of* the ground, so it
// composites over the surface and its alpha is capped well below opaque so the
// coastline stays readable underneath. Getting that backwards would wash the
// whole daylit hemisphere out.

struct Globals {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    sun_pos: vec4<f32>,
    sun_to_earth: vec4<f32>,
    solar_north: vec4<f32>,
    viewport: vec4<f32>,
    misc: vec4<f32>,
};

struct DrawData {
    model: mat4x4<f32>,
    basis: mat4x4<f32>,
    tint: vec4<f32>,
    tint2: vec4<f32>,
    // x: overall opacity (the layer's screen-size fade). yzw spare.
    params: vec4<f32>,
    style: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var land_tex: texture_2d<f32>;
@group(0) @binding(2) var sun_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(9) var prop_tex: texture_2d<f32>;
@group(1) @binding(0) var<uniform> d: DrawData;

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
    o.nrm = normalize((d.basis * vec4(pos, 0.0)).xyz);
    o.uv = uv;
    return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // The grid is row-major from the north pole and starts at 180° W, which is
    // the sphere's own UV convention, so no remapping is needed — the reason
    // `PropField` is laid out that way.
    let c = textureSample(prop_tex, samp, in.uv);
    var a = c.a * d.params.x;

    // Fade the shell out where it is edge-on. A sphere's silhouette is where a
    // surface texture is crossed at a grazing angle, and without this the rim
    // gathers into a hard bright ring that reads as a feature of the data
    // rather than of the geometry.
    let n = normalize(in.nrm);
    let to_eye = normalize(g.camera_pos.xyz - in.world);
    a = a * smoothstep(0.03, 0.34, abs(dot(n, to_eye)));

    if (a < 0.004) {
        discard;
    }
    // Premultiplied, matching the blend state this pipeline is built with.
    return vec4(c.rgb * a, a);
}
