//! Compile-time checks for the WGSL in `src/shaders/`.
//!
//! wgpu only builds these when the view is first opened, so without a test a
//! shader fault is a blank window (or a panic) at runtime rather than a red
//! test — and the WebGL2-only faults are invisible on a developer's machine,
//! where the native Vulkan/Metal backend accepts them.

/// Every shader the crate ships, in the order the views build them.
const SHADERS: &[(&str, &str)] = &[
    ("waterfall.wgsl", include_str!("../src/shaders/waterfall.wgsl")),
    ("waterfall_remap.wgsl", include_str!("../src/shaders/waterfall_remap.wgsl")),
    ("solar_body.wgsl", include_str!("../src/shaders/solar_body.wgsl")),
    ("solar_aurora.wgsl", include_str!("../src/shaders/solar_aurora.wgsl")),
    ("solar_cloud.wgsl", include_str!("../src/shaders/solar_cloud.wgsl")),
    ("solar_cloud_march.wgsl", include_str!("../src/shaders/solar_cloud_march.wgsl")),
    ("solar_cone.wgsl", include_str!("../src/shaders/solar_cone.wgsl")),
    ("solar_ring.wgsl", include_str!("../src/shaders/solar_ring.wgsl")),
    ("solar_tail.wgsl", include_str!("../src/shaders/solar_tail.wgsl")),
    ("solar_line.wgsl", include_str!("../src/shaders/solar_line.wgsl")),
    ("solar_sprite.wgsl", include_str!("../src/shaders/solar_sprite.wgsl")),
    ("solar_blit.wgsl", include_str!("../src/shaders/solar_blit.wgsl")),
];

fn parse(name: &str, src: &str) -> naga::Module {
    naga::front::wgsl::parse_str(src)
        .unwrap_or_else(|e| panic!("{name} failed to parse:\n{}", e.emit_to_string(src)))
}

/// A typo in the WGSL should be a test failure, not a blank window.
#[test]
fn shaders_compile_and_validate() {
    for (name, src) in SHADERS {
        let module = parse(name, src);
        let mut v = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        );
        v.validate(&module).unwrap_or_else(|e| panic!("{name} failed validation:\n{e:?}"));
    }
}

/// WebGL2 does not have `DownlevelFlags::BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED`,
/// so every uniform binding's type must be a multiple of 16 bytes or wgpu
/// rejects the pipeline outright. It is the size the *shader* declares that is
/// checked, so padding the matching Rust struct is not enough — and because
/// native backends do have the flag, the fault only ever shows up in the
/// browser client. Trailing padding fields keep it honest.
#[test]
fn uniform_structs_are_16_byte_multiples() {
    for (name, src) in SHADERS {
        let module = parse(name, src);
        for (_, var) in module.global_variables.iter() {
            if var.space != naga::AddressSpace::Uniform {
                continue;
            }
            let ty = &module.types[var.ty];
            let naga::TypeInner::Struct { span, .. } = ty.inner else { continue };
            assert_eq!(
                span % 16,
                0,
                "{name}: uniform `{}` has type `{}` of {span} bytes, which WebGL2 rejects; \
                 pad it out to {} bytes",
                var.name.as_deref().unwrap_or("?"),
                ty.name.as_deref().unwrap_or("?"),
                span.next_multiple_of(16),
            );
        }
    }
}
