// Our own pass: a head's texture to a texture the size of the view it is
// shown in. Slint then draws that texture 1:1, so what the user sees is
// exactly what this shader computed.
struct P {
    out_size: vec2<f32>,
    src_size: vec2<f32>,
    // 0: fit the view with linear filtering; 1: a whole-number scale with
    // nearest filtering (a scale below 1 falls back to fit).
    mode: u32,
    // 1 when the guest's cursor is drawn.
    cursor_on: u32,
    pad1: u32,
    pad2: u32,
    // The cursor image's top left in head pixels, and its size.
    cursor: vec4<f32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> p: P;
@group(0) @binding(3) var cur: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let avail = p.out_size / p.src_size;
    let fit = min(avail.x, avail.y);
    var s = fit;
    var whole = false;
    if (p.mode == 1u && fit >= 1.0) {
        s = floor(fit);
        whole = true;
    }
    let rect = p.src_size * s;
    let org = floor((p.out_size - rect) * 0.5);
    let q = (pos.xy - org) / s;
    let bg = vec4<f32>(0.0627, 0.0627, 0.0627, 1.0);
    if (q.x < 0.0 || q.y < 0.0 || q.x >= p.src_size.x || q.y >= p.src_size.y) {
        return bg;
    }
    var c: vec3<f32>;
    if (whole) {
        c = textureLoad(src, vec2<i32>(floor(q)), 0).rgb;
    } else {
        c = textureSampleLevel(src, samp, q / p.src_size, 0.0).rgb;
    }
    if (p.cursor_on == 1u) {
        let cx = floor(q.x) - p.cursor.x;
        let cy = floor(q.y) - p.cursor.y;
        if (cx >= 0.0 && cy >= 0.0 && cx < p.cursor.z && cy < p.cursor.w) {
            let t = textureLoad(cur, vec2<i32>(i32(cx), i32(cy)), 0);
            c = mix(c, t.rgb, t.a);
        }
    }
    return vec4<f32>(c, 1.0);
}
