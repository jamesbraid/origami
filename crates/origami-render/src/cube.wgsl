// The SGI tube cube: a chrome mirror over an optional scene-coloured band, after
// irix-theme's irix-cube-gl.js. Shading is linear; `finish` tone-maps and encodes.
struct U {
    mvp: mat4x4<f32>,
    nrm: mat4x4<f32>,
    // The studio's ground top and bottom, sky bottom and top levels.
    look: vec4<f32>,
    // Horizon band colour, and how much of it (0 for none).
    tint: vec4<f32>,
    // x: 0 studio, 1 equirect photo, 2 guest screen, 3 chrome horizon; y: 1 when the target
    // is sRGB and needs no encoding; z: granite texel size in pixels.
    mode: vec4<f32>,
    dark: vec4<f32>,
    light: vec4<f32>,
    fill: vec4<f32>,
    // The scene fading out: its mean colour, and in w how far the fade to
    // the current one has come (1 when there is none).
    fill_prev: vec4<f32>,
};

struct Grad {
    uv: vec2<f32>,
    dx: vec2<f32>,
    dy: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var photo: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var screen: texture_2d<f32>;
@group(0) @binding(4) var tile: texture_2d<f32>;
@group(0) @binding(5) var mean: texture_2d<f32>;
@group(0) @binding(6) var photo_prev: texture_2d<f32>;

const PI: f32 = 3.14159265;
const EDGE: f32 = 0.22;
const EDGE_SCENE: f32 = 0.5;
const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);
// Chrome's reflectance head-on, the key light's roughness and strength.
const F0: f32 = 0.86;
const ROUGH: f32 = 0.16;
const GLINT: f32 = 0.9;
// How much of the scene's average colour fills dark reflections.
const FILL: f32 = 0.25;
// The photo band's half height as a latitude, as the equirect assets are made.
const BAND: f32 = 0.6109;

@vertex
fn bg_vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = max(c, vec3<f32>(0.0));
    return select(1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3<f32>(0.0031308));
}

// Interleaved gradient noise, under half a level either way: hides banding.
fn dither(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715)))) - 0.5;
}

// Highlights roll off smoothly towards 1 above `knee`; everything below it is untouched.
fn shoulder(c: vec3<f32>) -> vec3<f32> {
    let knee = 0.6;
    let over = max(c - knee, vec3<f32>(0.0));
    return min(c, vec3<f32>(knee)) + (1.0 - knee) * (1.0 - exp(-over / (1.0 - knee)));
}

fn finish(c: vec3<f32>, pos: vec2<f32>) -> vec3<f32> {
    if (u.mode.y > 0.5) {
        return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return clamp(to_srgb(c) + dither(pos) / 255.0, vec3<f32>(0.0), vec3<f32>(1.0));
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

// `aa` is how far the reflected direction moves across a pixel: edges are softened to it.
fn studio(r: vec3<f32>, aa: f32) -> vec3<f32> {
    let h = r.y;
    let w = max(0.02, aa);
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

// The classic chrome-logo horizon: a dark ground, a crisp bright band at the
// horizon and a sky from near-white there to deep blue overhead.
fn horizon(r: vec3<f32>, aa: f32) -> vec3<f32> {
    let h = r.y;
    let e = max(0.01, aa);
    let sky = mix(vec3<f32>(0.97, 0.99, 1.0), vec3<f32>(0.08, 0.22, 0.68), pow(clamp(h, 0.0, 1.0), 1.7));
    let ground = mix(vec3<f32>(0.46, 0.41, 0.38), vec3<f32>(0.04, 0.04, 0.055), pow(clamp(-h, 0.0, 1.0), 0.85));
    var c = mix(ground, sky, smoothstep(-e, e, h));
    let line = band(h, -0.01, 0.16, 2.0 * e);
    c = mix(c, vec3<f32>(1.0), 0.7 * line);
    // The machine's colour tints the band subtly.
    return mix(c, u.tint.rgb, 0.4 * line * u.tint.a);
}

// Blender's world mapping, u = 0.5 - atan2(y, x) / 2pi with x right, y away from
// the camera and z up, so the direction to the camera is u = 0.75.
fn lonlat(r: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(atan2(r.z, r.x) / (2.0 * PI) + 0.5, 0.5 - asin(clamp(r.y, -1.0, 1.0)) / PI);
}

// The photo's coordinate and its screen-space derivatives, with the longitude's
// wrap taken out so the seam does not pick the blurriest mip.
fn photo_grad(r: vec3<f32>) -> Grad {
    let uv = lonlat(r);
    var dx = dpdx(uv);
    var dy = dpdy(uv);
    dx.x -= round(dx.x);
    dy.x -= round(dy.x);
    return Grad(uv, dx, dy);
}

fn equirect(g: Grad) -> vec3<f32> {
    let now = textureSampleGrad(photo, samp, g.uv, g.dx, g.dy).rgb;
    if (u.fill_prev.w >= 1.0) {
        return now;
    }
    return mix(textureSampleGrad(photo_prev, samp, g.uv, g.dx, g.dy).rgb, now, u.fill_prev.w);
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
        top += to_linear(textureSampleLevel(screen, samp, vec2<f32>(x, 0.01), 0.0).rgb) * 0.25;
        bot += to_linear(textureSampleLevel(screen, samp, vec2<f32>(x, 0.99), 0.0).rgb) * 0.25;
    }
    if (abs(lat) <= BAND) {
        let uv = vec2<f32>(1.0 - abs(2.0 * lon - 1.0), 0.5 - lat / (2.0 * BAND));
        return to_linear(textureSampleLevel(screen, samp, uv, 0.0).rgb);
    }
    let t = (abs(lat) - BAND) / (PI / 2.0 - BAND);
    if (lat > 0.0) {
        return mix(top, min(vec3<f32>(1.0), top * 1.15 + 0.12), t);
    }
    return bot * (1.0 - 0.6 * t);
}

// The scene's mean colour: the photograph's, set when it was loaded, or the
// screen's, from the 1 by 1 texture the downsample pass writes.
fn scene_average() -> vec3<f32> {
    if (u.mode.x == 1.0) {
        return mix(u.fill_prev.rgb, u.fill.rgb, u.fill_prev.w);
    }
    return to_linear(textureLoad(mean, vec2<i32>(0, 0), 0).rgb);
}

// A tight highlight from a key light up and to the left, so the chrome reads as polished.
fn glint(n: vec3<f32>, nv: f32) -> f32 {
    let l = normalize(vec3<f32>(-0.45, 0.62, 0.64));
    let h = normalize(l + vec3<f32>(0.0, 0.0, 1.0));
    let nh = saturate(dot(n, h));
    let a2 = ROUGH * ROUGH;
    let d = nh * nh * (a2 - 1.0) + 1.0;
    let ggx = a2 / (PI * d * d);
    let fres = F0 + (1.0 - F0) * pow(1.0 - saturate(dot(l, h)), 5.0);
    return ggx * fres * saturate(dot(n, l)) / (4.0 * max(nv, 0.1)) * GLINT;
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
    // Derivatives need uniform control flow, so they are taken before the branch.
    let g = photo_grad(r);
    let aa = 0.75 * max(length(dpdx(r)), length(dpdy(r)));
    var c: vec3<f32>;
    if (u.mode.x == 3.0) {
        c = to_linear(horizon(r, aa));
    } else if (u.mode.x == 1.0) {
        c = equirect(g);
    } else if (u.mode.x == 2.0) {
        c = on_screen(r);
    } else {
        c = to_linear(studio(r, aa));
    }
    // Schlick's Fresnel: polished chrome reflects most of what it sees, and all of it at grazing angles.
    c *= F0 + (1.0 - F0) * pow(saturate(1.0 - nv), 5.0);
    var edge = EDGE;
    if (u.mode.x == 1.0 || u.mode.x == 2.0) {
        // No tube reflects another here, which a ray tracer's would: lift dark
        // insides with the scene's mean, half desaturated against a violet cast.
        let avg = scene_average();
        let grey = vec3<f32>(dot(avg, LUMA));
        c += mix(avg, grey, 0.5) * FILL * (1.0 - min(1.0, dot(c, LUMA)));
        edge = EDGE_SCENE;
    }
    c *= mix(edge, 1.0, smoothstep(0.03, 0.17, nv));
    c += vec3<f32>(glint(n, nv));
    return vec4<f32>(finish(shoulder(c), v.pos.xy), 1.0);
}
