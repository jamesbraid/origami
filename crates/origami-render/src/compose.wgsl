// A head's changed lines, drawn from memory QEMU wrote, into the head's
// texture. The pass is clipped to the lines that changed, so everything else
// in the texture stays as it was. No copy of the pixels is made: the storage
// buffer is the region itself.
struct P {
    width: u32,
    height: u32,
    // Words from one row of the plane to the next.
    stride_words: u32,
    // 1 when row 0 of the plane is the bottom line of the picture.
    bottom_up: u32,
    // 1 when the pixel's bytes are r, g, b, unused (XB24) and not b, g, r.
    swap_rb: u32,
};

@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<uniform> p: P;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = u32(pos.x);
    var row = u32(pos.y);
    if (p.bottom_up == 1u) {
        row = p.height - 1u - row;
    }
    // x8r8g8b8 as a little-endian word: bytes b, g, r, unused.
    let c = unpack4x8unorm(src[row * p.stride_words + x]);
    if (p.swap_rb == 1u) {
        return vec4<f32>(c.x, c.y, c.z, 1.0);
    }
    return vec4<f32>(c.z, c.y, c.x, 1.0);
}
