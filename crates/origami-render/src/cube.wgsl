// The SGI tube cube: a chrome mirror over a granite backdrop. A port of the
// look of irix-theme's irix-cube-gl.js; the studio is analytic.
struct U {
    mvp: mat4x4<f32>,
    nrm: mat4x4<f32>,
    // The studio's ground top and bottom, sky bottom and top levels.
    look: vec4<f32>,
    // Horizon band colour, and how much of it (0 for none).
    tint: vec4<f32>,
    // x: 0 studio, 1 equirect photo, 2 guest screen; y: photo mip level;
    // z: granite texel size in pixels.
    mode: vec4<f32>,
    dark: vec4<f32>,
    light: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var photo: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var screen: texture_2d<f32>;
@group(0) @binding(4) var tile: texture_2d<f32>;

const PI: f32 = 3.14159265;
const EDGE: f32 = 0.22;
// The photo band's half height as a latitude, as the equirect assets are made.
const BAND: f32 = 0.6109;

@vertex
fn bg_vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn bg_fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let t = vec2<i32>(floor(pos.xy / u.mode.z));
    let m = textureLoad(tile, vec2<i32>(t.x & 255, t.y & 255), 0).r;
    return vec4<f32>(mix(u.dark.rgb, u.light.rgb, m), 1.0);
}

fn band(x: f32, a: f32, b: f32, w: f32) -> f32 {
    return smoothstep(a - w, a + w, x) * (1.0 - smoothstep(b - w, b + w, x));
}

fn halo(x: f32, c: f32, s: f32) -> f32 {
    let t = (x - c) / s;
    return exp(-t * t);
}

fn studio(r: vec3<f32>) -> vec3<f32> {
    let h = r.y;
    let w = 0.02;
    let ground = mix(u.look.x, u.look.y, smoothstep(-0.95, -0.08, h));
    let sky = mix(u.look.z, u.look.w, smoothstep(0.0, 0.8, h));
    var c = mix(ground, sky, smoothstep(-w, w, h));
    c += 0.45 * band(h, 0.03, 0.075, w);
    c = max(c, 0.95 * band(h, -0.6, -0.48, w) * band(r.x, -0.8, 0.8, w));
    c = max(c, 1.7 * band(h, 0.42, 0.74, w) * band(r.x, -0.75, 0.75, w));
    c = max(c, 1.6 * band(r.x, -0.97, -0.72, w) * band(h, -0.35, 0.92, w));
    c = max(c, 1.4 * band(r.x, 0.55, 0.63, w) * band(h, -0.55, 0.88, w));
    c += 0.16 * halo(h, 0.58, 0.22) * band(r.x, -0.95, 0.95, w)
        + 0.14 * halo(r.x, -0.84, 0.16) * smoothstep(-0.5, -0.3, h);
    var rgb = vec3<f32>(c);
    let m = band(h, -0.22, 0.02, 0.03) * u.tint.a;
    rgb = mix(rgb, u.tint.rgb * 0.85, m);
    return rgb;
}

// Blender's world mapping, u = 0.5 - atan2(y, x) / 2pi with x right, y away from
// the camera and z up, so the direction to the camera is u = 0.75.
fn equirect(r: vec3<f32>) -> vec3<f32> {
    let uv = vec2<f32>(atan2(r.z, r.x) / (2.0 * PI) + 0.5, 0.5 - asin(clamp(r.y, -1.0, 1.0)) / PI);
    return textureSampleLevel(photo, samp, uv, u.mode.y).rgb;
}

// The guest's screen round the horizon, mirrored across the back, over +-BAND
// of latitude; sky and ground fade to its top and bottom rows.
fn on_screen(r: vec3<f32>) -> vec3<f32> {
    let lat = asin(clamp(r.y, -1.0, 1.0));
    let lon = atan2(r.z, r.x) / (2.0 * PI) + 0.5;
    var top = vec3<f32>(0.0);
    var bot = vec3<f32>(0.0);
    for (var i = 0; i < 4; i++) {
        let x = (f32(i) + 0.5) / 4.0;
        top += textureSampleLevel(screen, samp, vec2<f32>(x, 0.01), 0.0).rgb * 0.25;
        bot += textureSampleLevel(screen, samp, vec2<f32>(x, 0.99), 0.0).rgb * 0.25;
    }
    if (abs(lat) <= BAND) {
        let uv = vec2<f32>(1.0 - abs(2.0 * lon - 1.0), 0.5 - lat / (2.0 * BAND));
        return textureSampleLevel(screen, samp, uv, 0.0).rgb;
    }
    let t = (abs(lat) - BAND) / (PI / 2.0 - BAND);
    if (lat > 0.0) {
        return mix(top, min(vec3<f32>(1.0), top * 1.15 + 0.12), t);
    }
    return bot * (1.0 - 0.6 * t);
}

struct V {
    @builtin(position) pos: vec4<f32>,
    @location(0) n: vec3<f32>,
};

@vertex
fn vs(@location(0) p: vec3<f32>, @location(1) n: vec3<f32>) -> V {
    var o: V;
    o.pos = u.mvp * vec4<f32>(p, 1.0);
    o.n = (u.nrm * vec4<f32>(n, 0.0)).xyz;
    return o;
}

@fragment
fn fs(v: V) -> @location(0) vec4<f32> {
    let n = normalize(v.n);
    let nv = abs(n.z);
    // reflect((0, 0, -1), n)
    let r = vec3<f32>(2.0 * n.z * n.x, 2.0 * n.z * n.y, -1.0 + 2.0 * n.z * n.z);
    var c: vec3<f32>;
    if (u.mode.x == 1.0) {
        c = equirect(r);
    } else if (u.mode.x == 2.0) {
        c = on_screen(r);
    } else {
        c = studio(r);
    }
    c *= mix(0.86, 1.0, pow(saturate(1.0 - nv), 5.0));
    c *= mix(EDGE, 1.0, smoothstep(0.03, 0.17, nv));
    return vec4<f32>(min(c, vec3<f32>(1.0)), 1.0);
}
