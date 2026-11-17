// A head's picture averaged down to the small texture the cube reflects.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// 4 by 4 taps of the bilinear filter per output pixel: a box average of the
// source pixels it covers, which a single tap would alias.
@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var sum = vec3<f32>(0.0);
    for (var j = 0; j < 4; j++) {
        for (var i = 0; i < 4; i++) {
            let uv = (floor(pos.xy) + (vec2<f32>(f32(i), f32(j)) + 0.5) / 4.0) / 256.0;
            sum += textureSampleLevel(src, samp, uv, 0.0).rgb;
        }
    }
    return vec4<f32>(sum / 16.0, 1.0);
}
