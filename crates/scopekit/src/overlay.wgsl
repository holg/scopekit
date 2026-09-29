// Draws the text texture again over the GPU views, inside scissor rects
// (the app's overlay cells). Same mapping as ratatui-wgpu's text blit: the
// text texture is stretched over the whole surface.

struct Uniforms {
    screen_size: vec2<f32>,
    use_srgb: u32,
    _pad: u32,
};

@group(0) @binding(0) var text: texture_2d<f32>;
@group(0) @binding(1) var text_sampler: sampler;
@group(0) @binding(2) var<uniform> u: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let v = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(v * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy / u.screen_size;
    // The text texture holds sRGB-encoded values; an sRGB surface re-encodes.
    let gamma = select(1.0, 2.2, u.use_srgb != 0u);
    let c = textureSample(text, text_sampler, uv);
    return vec4<f32>(pow(c.rgb, vec3<f32>(gamma)), c.a);
}
