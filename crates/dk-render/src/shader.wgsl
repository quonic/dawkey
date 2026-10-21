// SPDX-License-Identifier: AGPL-3.0-or-later
// One pipeline for every primitive: each instance is a quad, and the fragment
// shader evaluates a signed distance (or samples the glyph atlas) per pixel.

struct Globals {
    viewport: vec2f,
    _pad: vec2f,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;

const KIND_RECT: u32 = 0u;
const KIND_SHADOW: u32 = 1u;
const KIND_LINE: u32 = 2u;
const KIND_TRIANGLE: u32 = 3u;
const KIND_GLYPH: u32 = 4u;

struct Instance {
    // Quad to rasterize: x, y, w, h in physical pixels.
    @location(0) bounds: vec4f,
    @location(1) color: vec4f,
    @location(2) border_color: vec4f,
    // Corner radii: top-left, top-right, bottom-right, bottom-left.
    @location(3) radius: vec4f,
    // Rect/shadow: x, y, w, h. Line: x0, y0, x1, y1. Triangle: a, b.
    // Glyph: atlas x, y, w, h.
    @location(4) shape: vec4f,
    // Line: half width. Triangle: c.
    @location(5) extra: vec4f,
    // kind, border width, blur.
    @location(6) params: vec4f,
    // Clip: x0, y0, x1, y1.
    @location(7) clip: vec4f,
};

struct VOut {
    @builtin(position) pos: vec4f,
    @location(0) @interpolate(flat) bounds: vec4f,
    @location(1) @interpolate(flat) color: vec4f,
    @location(2) @interpolate(flat) border_color: vec4f,
    @location(3) @interpolate(flat) radius: vec4f,
    @location(4) @interpolate(flat) shape: vec4f,
    @location(5) @interpolate(flat) extra: vec4f,
    @location(6) @interpolate(flat) params: vec4f,
    @location(7) @interpolate(flat) clip: vec4f,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VOut {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0),
    );
    let px = inst.bounds.xy + corners[vi] * inst.bounds.zw;
    let ndc = vec2f(px.x / globals.viewport.x * 2.0 - 1.0, 1.0 - px.y / globals.viewport.y * 2.0);
    var out: VOut;
    out.pos = vec4f(ndc, 0.0, 1.0);
    out.bounds = inst.bounds;
    out.color = inst.color;
    out.border_color = inst.border_color;
    out.radius = inst.radius;
    out.shape = inst.shape;
    out.extra = inst.extra;
    out.params = inst.params;
    out.clip = inst.clip;
    return out;
}

fn sd_round_box(p: vec2f, half: vec2f, r: vec4f) -> f32 {
    var rad: f32;
    if (p.x < 0.0) {
        rad = select(r.w, r.x, p.y < 0.0);
    } else {
        rad = select(r.z, r.y, p.y < 0.0);
    }
    rad = min(rad, min(half.x, half.y));
    let q = abs(p) - half + vec2f(rad);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2f(0.0))) - rad;
}

fn sd_segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

fn sd_triangle(p: vec2f, p0: vec2f, p1: vec2f, p2: vec2f) -> f32 {
    let e0 = p1 - p0;
    let e1 = p2 - p1;
    let e2 = p0 - p2;
    let v0 = p - p0;
    let v1 = p - p1;
    let v2 = p - p2;
    let pq0 = v0 - e0 * clamp(dot(v0, e0) / dot(e0, e0), 0.0, 1.0);
    let pq1 = v1 - e1 * clamp(dot(v1, e1) / dot(e1, e1), 0.0, 1.0);
    let pq2 = v2 - e2 * clamp(dot(v2, e2) / dot(e2, e2), 0.0, 1.0);
    let s = sign(e0.x * e2.y - e0.y * e2.x);
    let d = min(min(
        vec2f(dot(pq0, pq0), s * (v0.x * e0.y - v0.y * e0.x)),
        vec2f(dot(pq1, pq1), s * (v1.x * e1.y - v1.y * e1.x))),
        vec2f(dot(pq2, pq2), s * (v2.x * e2.y - v2.y * e2.x)));
    return -sqrt(d.x) * sign(d.y);
}

fn premul(c: vec4f) -> vec4f {
    return vec4f(c.rgb * c.a, c.a);
}

fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4f {
    let p = in.pos.xy;
    if (p.x < in.clip.x || p.y < in.clip.y || p.x >= in.clip.z || p.y >= in.clip.w) {
        discard;
    }
    let kind = u32(in.params.x);
    switch kind {
        case KIND_RECT: {
            let half = in.shape.zw * 0.5;
            let d = sd_round_box(p - (in.shape.xy + half), half, in.radius);
            let border = in.params.y;
            if (border <= 0.0) {
                return premul(in.color) * coverage(d);
            }
            let inner = coverage(d + border);
            return mix(premul(in.border_color), premul(in.color), inner) * coverage(d);
        }
        case KIND_SHADOW: {
            let half = in.shape.zw * 0.5;
            let d = sd_round_box(p - (in.shape.xy + half), half, in.radius);
            let blur = max(in.params.z, 0.5);
            return premul(in.color) * (1.0 - smoothstep(-blur, blur, d));
        }
        case KIND_LINE: {
            let d = sd_segment(p, in.shape.xy, in.shape.zw) - in.extra.x;
            return premul(in.color) * coverage(d);
        }
        case KIND_TRIANGLE: {
            let d = sd_triangle(p, in.shape.xy, in.shape.zw, in.extra.xy);
            return premul(in.color) * coverage(d);
        }
        default: {
            let texel = in.shape.xy + floor(p - in.bounds.xy);
            let a = textureLoad(atlas, vec2i(texel), 0).r;
            return premul(in.color) * a;
        }
    }
}
