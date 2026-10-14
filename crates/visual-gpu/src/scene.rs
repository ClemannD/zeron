//! `ui.scene` on the GPU: physically based shading, soft shadows, glass that
//! refracts and splits light, and bloom, rendered offscreen and read back.
//!
//! Passes, per frame:
//! 1. shadow: the key light's depth over the geometry (orthographic);
//! 2. main: opaque surfaces into a multisampled HDR target with depth (GGX
//!    specular, sky/ground ambient, key light with PCF shadows, a fill
//!    light, glow), then lines and points unlit; resolved for sampling;
//! 3. glass: transmissive surfaces over that, refracting the resolved image
//!    per color channel (dispersion) under a Fresnel reflection;
//! 4. bloom: bright parts downsampled and blurred;
//! 5. composite: bloom added, a fine depth-edge outline, tonemapped to sRGB
//!    straight alpha, transparent wherever nothing was drawn.

use wgpu::util::DeviceExt;
use zeron_visual::{Camera, CameraFrame, Color, Scene, V3};

use crate::{FORMAT, Frame, gpu};

const HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SAMPLES: u32 = 4;
const SHADOW_SIZE: u32 = 2048;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    /// The face's own normal, for culling closed solids.
    face: [f32; 3],
    /// Linear color, straight alpha.
    color: [f32; 4],
    /// Roughness, metalness, glow, glass.
    mat: [f32; 4],
    /// Ior, dispersion, two-sided, kind (0 surface, 1 line, 2 point disc).
    opt: [f32; 4],
    /// Point-disc coordinates in -1..1.
    uv: [f32; 2],
}

impl Vertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x4,
        4 => Float32x4, 5 => Float32x4, 6 => Float32x2
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    /// Toward the key light.
    light: [f32; 4],
    sky: [f32; 4],
    ground: [f32; 4],
    /// Width, height, 1/width, 1/height (device pixels).
    viewport: [f32; 4],
    /// Near, far, the geometry's radius, unused.
    depth: [f32; 4],
    /// The camera's right, up, and forward, for screen-space refraction.
    right: [f32; 4],
    up: [f32; 4],
    forward: [f32; 4],
}

fn bytes<T>(value: &T) -> &[u8] {
    // SAFETY: plain-old-data `repr(C)` structs of `f32`s.
    unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
    }
}

fn slice_bytes<T>(values: &[T]) -> &[u8] {
    // SAFETY: as above, for a slice.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

pub(crate) struct Pipelines {
    globals_layout: wgpu::BindGroupLayout,
    glass_layout: wgpu::BindGroupLayout,
    post_layout: wgpu::BindGroupLayout,
    shadow: wgpu::RenderPipeline,
    opaque: wgpu::RenderPipeline,
    overlay: wgpu::RenderPipeline,
    glass: wgpu::RenderPipeline,
    bright: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    linear: wgpu::Sampler,
    shadow_sampler: wgpu::Sampler,
}

const SURFACE: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    light: vec4<f32>,
    sky: vec4<f32>,
    ground: vec4<f32>,
    viewport: vec4<f32>,
    depth: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    forward: vec4<f32>,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

struct In {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) face: vec3<f32>,
    @location(3) color: vec4<f32>,
    @location(4) mat: vec4<f32>,
    @location(5) opt: vec4<f32>,
    @location(6) uv: vec2<f32>,
};
struct Out {
    @builtin(position) clip: vec4<f32>,
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) face: vec3<f32>,
    @location(3) color: vec4<f32>,
    @location(4) mat: vec4<f32>,
    @location(5) opt: vec4<f32>,
    @location(6) uv: vec2<f32>,
};

@vertex fn vs(v: In) -> Out {
    var o: Out;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.pos = v.pos;
    o.normal = v.normal;
    o.face = v.face;
    o.color = v.color;
    o.mat = v.mat;
    o.opt = v.opt;
    o.uv = v.uv;
    return o;
}

@vertex fn vs_shadow(v: In) -> @builtin(position) vec4<f32> {
    return g.light_view_proj * vec4<f32>(v.pos, 1.0);
}

const PI: f32 = 3.14159265;

fn shadow(pos: vec3<f32>, n: vec3<f32>) -> f32 {
    let clip = g.light_view_proj * vec4<f32>(pos + n * 0.004 * g.depth.z, 1.0);
    let uv = vec2<f32>(clip.x * 0.5 + 0.5, 0.5 - clip.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) { return 1.0; }
    let texel = 1.0 / f32(textureDimensions(shadow_map).x);
    var lit = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            lit += textureSampleCompareLevel(shadow_map, shadow_sampler,
                uv + vec2<f32>(f32(x), f32(y)) * texel * 1.5, clip.z - 0.0015);
        }
    }
    return lit / 9.0;
}

fn ggx(n_h: f32, rough: f32) -> f32 {
    let a = rough * rough;
    let a2 = a * a;
    let d = n_h * n_h * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

fn smith(n_v: f32, n_l: f32, rough: f32) -> f32 {
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    return (n_v / (n_v * (1.0 - k) + k)) * (n_l / (n_l * (1.0 - k) + k));
}

fn fresnel(cos_t: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (1.0 - f0) * pow(1.0 - cos_t, 5.0);
}

fn environment(dir: vec3<f32>) -> vec3<f32> {
    return mix(g.ground.rgb, g.sky.rgb, smoothstep(-0.3, 0.6, dir.y));
}

// The shading normal turned toward the camera; w is 0 on the far side of a
// closed solid, which is not drawn.
fn facing(i: Out, v: vec3<f32>) -> vec4<f32> {
    var n = normalize(i.normal);
    if (i.opt.z < 0.5 && dot(i.face, v) < 0.0) { return vec4<f32>(0.0); }
    if (dot(n, v) < 0.0) { n = -n; }
    return vec4<f32>(n, 1.0);
}

fn shade(i: Out, n: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let albedo = i.color.rgb;
    let rough = clamp(i.mat.x, 0.04, 1.0);
    let metal = i.mat.y;
    let l = normalize(g.light.xyz);
    let h = normalize(l + v);
    let n_l = max(dot(n, l), 0.0);
    let n_v = max(dot(n, v), 0.001);
    let n_h = max(dot(n, h), 0.0);
    let f0 = mix(vec3<f32>(0.04), albedo, metal);
    let f = fresnel(max(dot(h, v), 0.0), f0);
    let spec = ggx(n_h, rough) * smith(n_v, n_l, rough) * f / max(4.0 * n_v * n_l, 0.001);
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metal);
    let key = (kd * albedo / PI + spec) * n_l * 3.2 * shadow(i.pos, n);
    let fill_dir = normalize(vec3<f32>(-l.x, 0.35, -l.z));
    let fill = kd * albedo / PI * max(dot(n, fill_dir), 0.0) * 0.9;
    let r = reflect(-v, n);
    let env_f = fresnel(n_v, f0);
    let ambient = kd * albedo * environment(n) * 0.85
        + env_f * environment(r) * (1.0 - rough * 0.85) * 0.8;
    return key + fill + ambient + albedo * i.mat.z;
}

@fragment fn fs_opaque(i: Out) -> @location(0) vec4<f32> {
    let v = normalize(g.eye.xyz - i.pos);
    let f = facing(i, v);
    if (f.w == 0.0) { discard; }
    return vec4<f32>(shade(i, f.xyz, v), 1.0);
}

@fragment fn fs_overlay(i: Out) -> @location(0) vec4<f32> {
    if (i.opt.w > 1.5 && dot(i.uv, i.uv) > 1.0) { discard; }
    let c = i.color.rgb * (1.0 + i.mat.z);
    return vec4<f32>(c * i.color.a, i.color.a);
}
"#;

const GLASS: &str = r#"
@group(1) @binding(0) var behind: texture_2d<f32>;
@group(1) @binding(1) var behind_sampler: sampler;

@fragment fn fs_glass(i: Out) -> @location(0) vec4<f32> {
    let v = normalize(g.eye.xyz - i.pos);
    let f = facing(i, v);
    if (f.w == 0.0) { discard; }
    let n = f.xyz;
    let ior = max(i.opt.x, 1.0);
    let spread = i.opt.y * 0.09;
    let screen = i.clip.xy * g.viewport.zw;
    // Bend what's behind by the surface's tilt across the view, a little
    // more for each shorter wavelength.
    let view_n = vec2<f32>(dot(n, g.right.xyz), -dot(n, g.up.xyz));
    var seen = vec3<f32>(0.0);
    var cover = 0.0;
    let iors = vec3<f32>(ior - spread, ior, ior + spread);
    for (var c = 0; c < 3; c++) {
        let bend = (1.0 - 1.0 / iors[c]) * 0.34;
        let s = textureSampleLevel(behind, behind_sampler, screen - view_n * bend, 0.0);
        seen[c] = s[c];
        cover = max(cover, s.a);
    }
    let tint = mix(vec3<f32>(1.0), i.color.rgb, 0.35);
    let f0 = pow((ior - 1.0) / (ior + 1.0), 2.0);
    let n_v = max(dot(n, v), 0.0);
    let fr = f0 + (1.0 - f0) * pow(1.0 - n_v, 5.0);
    let r = reflect(-v, n);
    let l = normalize(g.light.xyz);
    let glint = pow(max(dot(r, l), 0.0), 220.0) * 6.0;
    let reflection = environment(r) * 1.2 + vec3<f32>(glint);
    let through = seen * tint;
    let glass = clamp(i.mat.w, 0.0, 1.0);
    // Where nothing stands behind the glass, it shows its reflection and a
    // faint body tint over the chat.
    let body = 0.12 + 0.25 * (1.0 - glass);
    let alpha = clamp(max(cover, fr + body + glint), 0.0, 1.0);
    let color = through + (reflection - through) * fr + i.color.rgb * body * (1.0 - cover)
        + i.color.rgb * i.mat.z;
    return vec4<f32>(color, alpha);
}
"#;

const POST: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> post: vec4<f32>;
@group(0) @binding(3) var bloom: texture_2d<f32>;
@group(0) @binding(4) var depth: texture_depth_multisampled_2d;

struct Out { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: Out;
    o.clip = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}

fn luma(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722)); }

@fragment fn fs_bright(i: Out) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(source, source_sampler, i.uv, 0.0);
    let over = max(luma(c.rgb) - 1.0, 0.0);
    return vec4<f32>(c.rgb * (over / max(luma(c.rgb), 0.0001)), 1.0);
}

// post.xy: one texel step along the blur direction.
@fragment fn fs_blur(i: Out) -> @location(0) vec4<f32> {
    let w = array<f32, 5>(0.227, 0.194, 0.121, 0.054, 0.016);
    var sum = textureSampleLevel(source, source_sampler, i.uv, 0.0).rgb * w[0];
    for (var k = 1; k < 5; k++) {
        let o = post.xy * f32(k) * 1.5;
        sum += textureSampleLevel(source, source_sampler, i.uv + o, 0.0).rgb * w[k];
        sum += textureSampleLevel(source, source_sampler, i.uv - o, 0.0).rgb * w[k];
    }
    return vec4<f32>(sum, 1.0);
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

const NEIGHBOURS = array<vec2<i32>, 4>(vec2<i32>(1, 0), vec2<i32>(-1, 0), vec2<i32>(0, 1), vec2<i32>(0, -1));

// post.x: bloom strength.
@fragment fn fs_composite(i: Out) -> @location(0) vec4<f32> {
    let c = textureSampleLevel(source, source_sampler, i.uv, 0.0);
    let glow = textureSampleLevel(bloom, source_sampler, i.uv, 0.0).rgb * post.x;
    // A fine outline where depth jumps: silhouettes and folds.
    let size = vec2<i32>(textureDimensions(depth));
    let at = clamp(vec2<i32>(i.clip.xy), vec2<i32>(0), size - 1);
    let z = textureLoad(depth, at, 0);
    var edge = 0.0;
    if (z < 1.0) {
        for (var k = 0; k < 4; k++) {
            let zn = textureLoad(depth, clamp(at + NEIGHBOURS[k], vec2<i32>(0), size - 1), 0);
            if (zn >= 1.0 || abs(zn - z) > 0.004 * (1.0 - z) + 0.0004) { edge = 1.0; }
        }
    }
    let alpha = clamp(c.a + luma(glow), 0.0, 1.0);
    if (alpha <= 0.0001) { return vec4<f32>(0.0); }
    var straight = (c.rgb + glow) / alpha;
    straight = straight * (1.0 - 0.18 * edge);
    return vec4<f32>(to_srgb(aces(straight)), alpha);
}
"#;

impl Pipelines {
    fn new(device: &wgpu::Device) -> Self {
        let uniform = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture = |binding, sample_type, multisampled| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled,
            },
            count: None,
        };
        let sampler = |binding, kind| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(kind),
            count: None,
        };
        let float = wgpu::TextureSampleType::Float { filterable: true };
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene globals"),
            entries: &[
                uniform(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                texture(1, wgpu::TextureSampleType::Depth, false),
                sampler(2, wgpu::SamplerBindingType::Comparison),
            ],
        });
        let glass_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene behind glass"),
            entries: &[
                texture(0, float, false),
                sampler(1, wgpu::SamplerBindingType::Filtering),
            ],
        });
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene post"),
            entries: &[
                texture(0, float, false),
                sampler(1, wgpu::SamplerBindingType::Filtering),
                uniform(2, wgpu::ShaderStages::FRAGMENT),
                texture(3, float, false),
                texture(4, wgpu::TextureSampleType::Depth, true),
            ],
        });
        let surface = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene surfaces"),
            source: wgpu::ShaderSource::Wgsl(SURFACE.into()),
        });
        let glass_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene glass"),
            source: wgpu::ShaderSource::Wgsl(format!("{SURFACE}{GLASS}").into()),
        });
        let post = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene post"),
            source: wgpu::ShaderSource::Wgsl(POST.into()),
        });
        let layout = |layouts: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: layouts,
                immediate_size: 0,
            })
        };
        let surface_layout = layout(&[Some(&globals_layout)]);
        let glass_pipeline_layout = layout(&[Some(&globals_layout), Some(&glass_layout)]);
        let post_pipeline_layout = layout(&[Some(&post_layout)]);
        let msaa = wgpu::MultisampleState {
            count: SAMPLES,
            ..Default::default()
        };
        let depth = |write: bool, bias: wgpu::DepthBiasState| {
            Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(write),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias,
            })
        };
        let surface_pipeline = |label: &str,
                                module: &wgpu::ShaderModule,
                                layout: &wgpu::PipelineLayout,
                                fragment: &str,
                                blend: Option<wgpu::BlendState>,
                                depth_stencil: Option<wgpu::DepthStencilState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Vertex::layout()],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil,
                multisample: msaa,
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque = surface_pipeline(
            "scene opaque",
            &surface,
            &surface_layout,
            "fs_opaque",
            None,
            depth(true, Default::default()),
        );
        let overlay = surface_pipeline(
            "scene lines and points",
            &surface,
            &surface_layout,
            "fs_overlay",
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            depth(
                false,
                wgpu::DepthBiasState {
                    constant: -4,
                    slope_scale: -1.0,
                    clamp: 0.0,
                },
            ),
        );
        let glass = surface_pipeline(
            "scene glass",
            &glass_module,
            &glass_pipeline_layout,
            "fs_glass",
            None,
            depth(true, Default::default()),
        );
        let shadow = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene shadow"),
            layout: Some(&surface_layout),
            vertex: wgpu::VertexState {
                module: &surface,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[Vertex::layout()],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: Default::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });
        let post_pipeline = |label: &str, entry: &str, format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&post_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &post,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &post,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            bright: post_pipeline("scene bloom bright", "fs_bright", HDR),
            blur: post_pipeline("scene bloom blur", "fs_blur", HDR),
            composite: post_pipeline("scene composite", "fs_composite", FORMAT),
            globals_layout,
            glass_layout,
            post_layout,
            shadow,
            opaque,
            overlay,
            glass,
            linear: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("scene linear"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            }),
            shadow_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("scene shadow"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                compare: Some(wgpu::CompareFunction::LessEqual),
                ..Default::default()
            }),
        }
    }
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(a: V3) -> V3 {
    let len = dot(a, a).sqrt();
    if len < 1e-9 {
        [0.0, 1.0, 0.0]
    } else {
        scale(a, 1.0 / len)
    }
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear(rgba: [f32; 4]) -> [f32; 4] {
    [
        to_linear(rgba[0]),
        to_linear(rgba[1]),
        to_linear(rgba[2]),
        rgba[3],
    ]
}

/// Rows of a matrix into WGSL's column-major layout.
fn columns(rows: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut cols = [[0.0; 4]; 4];
    for (i, row) in rows.iter().enumerate() {
        for (j, value) in row.iter().enumerate() {
            cols[j][i] = *value;
        }
    }
    cols
}

/// The world-space key light, from above and to the side, so shadows fall
/// toward the viewer's default angle and ground the geometry.
const KEY_LIGHT: V3 = [-0.45, 0.85, 0.35];

struct Geometry {
    /// Opaque surfaces; the first `shadowed` vertices cast shadows (glowing
    /// surfaces, which are lights, come after and don't).
    opaque: Vec<Vertex>,
    shadowed: usize,
    glass: Vec<Vertex>,
    overlay: Vec<Vertex>,
}

fn geometry(scene: &Scene, frame: &CameraFrame, rgba: &dyn Fn(Color) -> [f32; 4]) -> Geometry {
    let mut glowing = Vec::new();
    let mut out = Geometry {
        opaque: Vec::new(),
        shadowed: 0,
        glass: Vec::new(),
        overlay: Vec::new(),
    };
    let mut glass_faces: Vec<(f32, Vec<Vertex>)> = Vec::new();
    for face in scene.face_views() {
        let m = face.material;
        let color = linear(rgba(face.color));
        let vertex = |i: usize| Vertex {
            pos: face.corners[i],
            normal: face.normals.map_or(face.normal, |n| n[i]),
            face: face.normal,
            color,
            mat: [m.roughness, m.metalness, m.glow, m.glass],
            opt: [
                m.ior,
                m.dispersion,
                if face.two_sided { 1.0 } else { 0.0 },
                0.0,
            ],
            uv: [0.0; 2],
        };
        let mut tris = Vec::with_capacity((face.corners.len() - 2) * 3);
        for i in 1..face.corners.len() - 1 {
            tris.extend([vertex(0), vertex(i), vertex(i + 1)]);
        }
        if m.glass > 0.0 || color[3] < 0.999 {
            let centroid = face.corners.iter().fold([0.0; 3], |acc, c| add(acc, *c));
            let centroid = scale(centroid, 1.0 / face.corners.len() as f32);
            glass_faces.push((dot(sub(centroid, frame.eye), frame.forward), tris));
        } else if m.glow > 0.0 {
            glowing.extend(tris);
        } else {
            out.opaque.extend(tris);
        }
    }
    out.shadowed = out.opaque.len();
    out.opaque.extend(glowing);
    // Glass far to near, so nearer panes cover farther ones.
    glass_faces.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.glass = glass_faces.into_iter().flat_map(|(_, t)| t).collect();

    // Lines and points face the camera, sized in screen pixels.
    let world_per_pixel =
        |p: V3| dot(sub(p, frame.eye), frame.forward).max(frame.near) / frame.focal;
    let flat = |pos: V3, color: [f32; 4], kind: f32, uv: [f32; 2]| Vertex {
        pos,
        normal: [0.0, 1.0, 0.0],
        face: [0.0, 1.0, 0.0],
        color,
        mat: [1.0, 0.0, 0.0, 0.0],
        opt: [1.0, 0.0, 1.0, kind],
        uv,
    };
    for (a, b, color, width) in scene.line_views() {
        let color = linear(rgba(color));
        let dir = normalize(sub(b, a));
        let side_a = normalize(cross(dir, sub(a, frame.eye)));
        let side_b = normalize(cross(dir, sub(b, frame.eye)));
        let wa = scale(side_a, width.max(1.0) * 0.5 * world_per_pixel(a));
        let wb = scale(side_b, width.max(1.0) * 0.5 * world_per_pixel(b));
        let quad = [add(a, wa), add(b, wb), sub(b, wb), sub(a, wa)];
        for i in [0, 1, 2, 0, 2, 3] {
            out.overlay.push(flat(quad[i], color, 1.0, [0.0; 2]));
        }
    }
    for (at, radius, color) in scene.point_views() {
        let color = linear(rgba(color));
        let r = radius * world_per_pixel(at);
        let (x, y) = (scale(frame.right, r), scale(frame.up, r));
        let corners = [
            (add(sub(at, x), y), [-1.0, 1.0]),
            (add(add(at, x), y), [1.0, 1.0]),
            (sub(add(at, x), y), [1.0, -1.0]),
            (sub(sub(at, x), y), [-1.0, -1.0]),
        ];
        for i in [0, 1, 2, 0, 2, 3] {
            out.overlay.push(flat(corners[i].0, color, 2.0, corners[i].1));
        }
    }
    out
}

/// Render `scene` on the GPU for a canvas `width` by `height` logical pixels
/// at `pixel_scale`. `None` without a GPU (or geometry), so the caller can
/// fall back to the software renderer.
pub fn render_scene(
    scene: &Scene,
    camera: Camera,
    width: f32,
    height: f32,
    pixel_scale: f32,
    rgba: &dyn Fn(Color) -> [f32; 4],
) -> Option<Frame> {
    let gpu = gpu()?;
    let frame = scene.camera_frame(camera, width, height)?;
    let (w, h) = (
        (width * pixel_scale).round().max(1.0) as u32,
        (height * pixel_scale).round().max(1.0) as u32,
    );
    let pipelines = gpu.scene.get_or_init(|| Pipelines::new(&gpu.device));
    let device = &gpu.device;
    let geometry = geometry(scene, &frame, rgba);

    // Camera: the same framing and projection as the software renderer.
    let distance = dot(sub(frame.center, frame.eye), sub(frame.center, frame.eye)).sqrt();
    let near = frame.near;
    let far = distance + frame.radius * 2.0;
    let (sx, sy) = (frame.focal / (width / 2.0), frame.focal / (height / 2.0));
    let (a, b) = (far / (far - near), -far * near / (far - near));
    let row = |axis: V3, s: f32| {
        [
            axis[0] * s,
            axis[1] * s,
            axis[2] * s,
            -dot(axis, frame.eye) * s,
        ]
    };
    let f = frame.forward;
    let view_proj = columns([
        row(frame.right, sx),
        row(frame.up, sy),
        [f[0] * a, f[1] * a, f[2] * a, -dot(f, frame.eye) * a + b],
        row(f, 1.0),
    ]);
    // The key light's orthographic view, fitted to the geometry.
    let light = normalize(KEY_LIGHT);
    let light_forward = scale(light, -1.0);
    let light_right = normalize(cross(light_forward, [0.0, 0.0, 1.0]));
    let light_up = cross(light_right, light_forward);
    let r = frame.radius * 1.1;
    let c = frame.center;
    let light_view_proj = columns([
        [
            light_right[0] / r,
            light_right[1] / r,
            light_right[2] / r,
            -dot(light_right, c) / r,
        ],
        [
            light_up[0] / r,
            light_up[1] / r,
            light_up[2] / r,
            -dot(light_up, c) / r,
        ],
        [
            light_forward[0] / (2.0 * r),
            light_forward[1] / (2.0 * r),
            light_forward[2] / (2.0 * r),
            (-dot(light_forward, c) + r) / (2.0 * r),
        ],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let globals = Globals {
        view_proj,
        light_view_proj,
        eye: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
        light: [light[0], light[1], light[2], 0.0],
        sky: [0.62, 0.66, 0.74, 0.0],
        ground: [0.26, 0.24, 0.22, 0.0],
        viewport: [w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32],
        depth: [near, far, frame.radius, 0.0],
        right: [frame.right[0], frame.right[1], frame.right[2], 0.0],
        up: [frame.up[0], frame.up[1], frame.up[2], 0.0],
        forward: [f[0], f[1], f[2], 0.0],
    };
    let globals_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("scene globals"),
        contents: bytes(&globals),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    // Targets.
    use wgpu::TextureUsages as U;
    let texture = |label: &str, size: (u32, u32), format, samples, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let sampled = U::RENDER_ATTACHMENT | U::TEXTURE_BINDING;
    let shadow_map = texture(
        "scene shadow map",
        (SHADOW_SIZE, SHADOW_SIZE),
        DEPTH,
        1,
        sampled,
    );
    let msaa_color = texture("scene color", (w, h), HDR, SAMPLES, U::RENDER_ATTACHMENT);
    let msaa_depth = texture("scene depth", (w, h), DEPTH, SAMPLES, sampled);
    let opaque_resolved = texture("scene behind glass", (w, h), HDR, 1, sampled);
    let lit = texture("scene lit", (w, h), HDR, 1, sampled);
    let half = ((w / 2).max(1), (h / 2).max(1));
    let bloom_a = texture("scene bloom", half, HDR, 1, sampled);
    let bloom_b = texture("scene bloom", half, HDR, 1, sampled);
    let output = gpu.target(w, h, "scene output");
    let view = |t: &wgpu::Texture| t.create_view(&Default::default());

    let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("scene globals"),
        layout: &pipelines.globals_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view(&shadow_map)),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&pipelines.shadow_sampler),
            },
        ],
    });
    let vertices = |label: &str, data: &[Vertex]| {
        (!data.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: slice_bytes(data),
                usage: wgpu::BufferUsages::VERTEX,
            })
        })
    };
    let opaque = vertices("scene opaque", &geometry.opaque);
    let glass = vertices("scene glass", &geometry.glass);
    let overlay = vertices("scene overlay", &geometry.overlay);
    let draw = |pass: &mut wgpu::RenderPass, buffer: &Option<wgpu::Buffer>, count: usize| {
        if let Some(buffer) = buffer {
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..count as u32, 0..1);
        }
    };

    let mut encoder = device.create_command_encoder(&Default::default());
    // 1. Shadow. Its bind group (with the sampled map) is unused here: the
    // shadow vertex stage only reads the globals.
    let shadow_globals = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("scene shadow globals"),
        layout: &pipelines.globals_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view(&lit_dummy_depth(device))),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&pipelines.shadow_sampler),
            },
        ],
    });
    {
        let shadow_view = view(&shadow_map);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene shadow"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &shadow_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipelines.shadow);
        pass.set_bind_group(0, &shadow_globals, &[]);
        draw(&mut pass, &opaque, geometry.shadowed);
    }
    let color_view = view(&msaa_color);
    let depth_view = view(&msaa_depth);
    // 2. Opaque surfaces, then lines and points.
    {
        let resolve = view(&opaque_resolved);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: Some(&resolve),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &globals_bind, &[]);
        pass.set_pipeline(&pipelines.opaque);
        draw(&mut pass, &opaque, geometry.opaque.len());
        pass.set_pipeline(&pipelines.overlay);
        draw(&mut pass, &overlay, geometry.overlay.len());
    }
    // 3. Glass, refracting the resolved opaque image.
    let shaded = if glass.is_some() {
        let behind_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene behind glass"),
            layout: &pipelines.glass_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(&opaque_resolved)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.linear),
                },
            ],
        });
        let resolve = view(&lit);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene glass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: Some(&resolve),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &globals_bind, &[]);
        pass.set_bind_group(1, &behind_bind, &[]);
        pass.set_pipeline(&pipelines.glass);
        draw(&mut pass, &glass, geometry.glass.len());
        &lit
    } else {
        &opaque_resolved
    };

    // 4–5. Bloom and composite.
    let post_uniform = |values: [f32; 4]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene post"),
            contents: slice_bytes(&values),
            usage: wgpu::BufferUsages::UNIFORM,
        })
    };
    let depth_sample_view = msaa_depth.create_view(&wgpu::TextureViewDescriptor {
        aspect: wgpu::TextureAspect::DepthOnly,
        ..Default::default()
    });
    let post_bind = |source: &wgpu::Texture, uniform: &wgpu::Buffer, bloom: &wgpu::Texture| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene post"),
            layout: &pipelines.post_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(source)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.linear),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&view(bloom)),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&depth_sample_view),
                },
            ],
        })
    };
    let fullscreen = |encoder: &mut wgpu::CommandEncoder,
                      target: &wgpu::Texture,
                      pipeline: &wgpu::RenderPipeline,
                      bind: &wgpu::BindGroup| {
        let target = view(target);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene post"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    };
    let zero = post_uniform([0.0; 4]);
    let step_x = post_uniform([1.0 / half.0 as f32, 0.0, 0.0, 0.0]);
    let step_y = post_uniform([0.0, 1.0 / half.1 as f32, 0.0, 0.0]);
    fullscreen(
        &mut encoder,
        &bloom_a,
        &pipelines.bright,
        &post_bind(shaded, &zero, &bloom_b),
    );
    for _ in 0..2 {
        fullscreen(
            &mut encoder,
            &bloom_b,
            &pipelines.blur,
            &post_bind(&bloom_a, &step_x, &lit_dummy(device)),
        );
        fullscreen(
            &mut encoder,
            &bloom_a,
            &pipelines.blur,
            &post_bind(&bloom_b, &step_y, &lit_dummy(device)),
        );
    }
    let strength = post_uniform([0.9, 0.0, 0.0, 0.0]);
    fullscreen(
        &mut encoder,
        &output,
        &pipelines.composite,
        &post_bind(shaded, &strength, &bloom_a),
    );
    Some(gpu.read_back(encoder, &output, false))
}

/// A 1×1 texture for bind slots a pass doesn't read (a texture can't be both
/// the target and a binding of one pass).
fn lit_dummy(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene unused"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// As [`lit_dummy`], for the shadow map slot while the shadow map is drawn.
fn lit_dummy_depth(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scene unused depth"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_visual::Token;

    fn palette(color: Color) -> [f32; 4] {
        match color.token {
            Token::Chart(0) => [0.3, 0.6, 0.95, 1.0],
            Token::Chart(1) => [0.95, 0.45, 0.35, 1.0],
            Token::Chart(2) => [0.4, 0.85, 0.5, 1.0],
            Token::Chart(3) => [0.95, 0.8, 0.3, 1.0],
            _ => [0.75, 0.75, 0.78, 1.0],
        }
    }

    fn room() -> Scene {
        let mut visual = zeron_visual::Visual::load(
            r#"
            return function()
              return ui.scene { height = 300, draw = function(s)
                s:box { at = { 0, -0.05, 0 }, size = { 6, 0.1, 4 }, color = "muted" }
                s:box { at = { -1.6, 0.5, 0 }, size = { 1, 1, 1 }, color = "chart1", roughness = 0.35 }
                s:sphere { at = { 0, 0.6, 0.4 }, radius = 0.6, color = "chart2", metal = 1, roughness = 0.25 }
                s:box { at = { 1.6, 0.6, 0 }, size = { 0.9, 1.2, 0.9 }, color = "chart3", glass = true, dispersion = 1 }
                s:sphere { at = { 1.6, 1.6, -1 }, radius = 0.2, color = "chart4", glow = 4 }
              end }
            end
            "#,
            1,
        )
        .unwrap();
        let tree = visual.render().unwrap();
        let zeron_visual::NodeKind::Scene(node) = tree.kind else {
            panic!("a scene")
        };
        visual.draw_scene(node.draw, 0.0).unwrap()
    }

    #[test]
    fn a_room_renders_lit_with_glass_and_glow() {
        if !crate::available() {
            return;
        }
        let scene = room();
        let camera = Camera {
            yaw: 0.6,
            pitch: 0.45,
            zoom: 1.0,
        };
        // The first frame also makes the device and compiles pipelines.
        render_scene(&scene, camera, 720.0, 360.0, 2.0, &palette).unwrap();
        let start = std::time::Instant::now();
        let frame = render_scene(&scene, camera, 720.0, 360.0, 2.0, &palette).unwrap();
        eprintln!(
            "gpu scene {}x{} in {:?}",
            frame.width,
            frame.height,
            start.elapsed()
        );
        if let Ok(path) = std::env::var("ZERON_VISUAL_DUMP") {
            crate::dump_ppm(&frame, &path);
        }
        assert_eq!((frame.width, frame.height), (1440, 720));
        let drawn = frame.bgra.chunks_exact(4).filter(|p| p[3] > 0).count();
        let total = (frame.width * frame.height) as usize;
        assert!(drawn > total / 8, "geometry fills the frame: {drawn}");
        assert!(drawn < total, "and the background stays clear");
    }
}
