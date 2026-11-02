//! Rule `stand-in-3d`. The game binds every pack texture as 2D and loads no custom texture files
//! (nightly 7aa0e28), so a program that samples a `sampler3D` is refused. Each `sampler3D` becomes
//! a `hz_tex3d` (a struct holding an id) and `texture`/`textureLod`/`texelFetch`/`textureSize`
//! get overloads for it, so the pack's calls (and its macros around them) stay as written. What
//! a lookup returns is built from the pack's own data file (`texture.<stage>.<name> = file.dat
//! TEXTURE_3D …`):
//!
//! - smooth data (a lookup table such as an atmosphere's): a shrunk copy, at most 256 texels,
//!   read with linear filtering;
//! - noise volumes (clouds, fog): tiling value noise per channel, at the feature size measured in
//!   the file and scaled to the file's average and contrast. The shapes differ from the pack's
//!   noise; the amount and scale are close.
//!
//! A `sampler3D` with no data for the program's stage reads (0, 0, 0, 1), as an incomplete
//! texture does in GL.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Edits, insert_decl, sources, stages_using};
use crate::shaderport::glsl::{comment_ranges, find_word, in_comment, is_ident_byte};
use crate::shaderport::load::Eval;

/// A 3D texture's data, as floats (`ch` per texel, x fastest).
struct Volume {
    file: String,
    size: [usize; 3],
    ch: usize,
    texels: Vec<f32>,
}

impl Volume {
    fn at(&self, x: usize, y: usize, z: usize, c: usize) -> f32 {
        self.texels[((z * self.size[1] + y) * self.size[0] + x) * self.ch + c]
    }
}

/// `texture.<stage>.<name>[.<n>]` and `customTexture.<name>` entries for 3D files:
/// (stage or `*`, sampler name, file, format words).
fn entries(files: &Files) -> Vec<(String, String, String, Vec<String>)> {
    let Some(props) = files.text("shaders.properties") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in props.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        let words: Vec<String> = value.split_whitespace().map(str::to_owned).collect();
        if words.len() < 2 || words[1] != "TEXTURE_3D" {
            continue;
        }
        let parts: Vec<&str> = key.split('.').collect();
        let (stage, name) = match parts.as_slice() {
            ["texture", stage, name, ..] => (stage.to_string(), name.to_string()),
            ["customTexture", name] => ("*".into(), name.to_string()),
            _ => continue,
        };
        out.push((stage, name, words[0].clone(), words[1..].to_vec()));
    }
    out
}

fn half(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = (h >> 10) & 0x1f;
    let m = (h & 0x3ff) as f32;
    s * match e {
        0 => m * 2f32.powi(-24),
        31 => f32::MAX,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e as i32 - 15),
    }
}

/// Reads `file` as `<internal> <w> <h> <d> <pixel format> <pixel type>`.
fn read_volume(files: &Files, file: &str, words: &[String]) -> Option<Volume> {
    if words.len() < 7 {
        return None;
    }
    let dims: Vec<usize> = words[2..5].iter().map(|w| w.parse().ok()).collect::<Option<_>>()?;
    let ch = match words[5].trim_end_matches("_INTEGER") {
        "RED" | "R" => 1,
        "RG" => 2,
        "RGB" | "BGR" => 3,
        "RGBA" | "BGRA" => 4,
        _ => return None,
    };
    let path = file.trim_start_matches('/');
    let data = files.entries.get(&format!("shaders/{path}"))?;
    let n = dims[0] * dims[1] * dims[2] * ch;
    let texels: Vec<f32> = match words[6].as_str() {
        "UNSIGNED_BYTE" => data.get(..n)?.iter().map(|&b| b as f32 / 255.0).collect(),
        "BYTE" => data.get(..n)?.iter().map(|&b| (b as i8 as f32 / 127.0).max(-1.0)).collect(),
        "UNSIGNED_SHORT" => data.get(..n * 2)?.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as f32 / 65535.0).collect(),
        "HALF_FLOAT" => data.get(..n * 2)?.chunks(2).map(|c| half(u16::from_le_bytes([c[0], c[1]]))).collect(),
        "FLOAT" => data.get(..n * 4)?.chunks(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
        _ => return None,
    };
    let mut texels = texels;
    if words[5].starts_with("BGR") {
        for t in texels.chunks_mut(ch) {
            t.swap(0, 2);
        }
    }
    Some(Volume { file: file.to_owned(), size: [dims[0], dims[1], dims[2]], ch, texels })
}

/// Deterministic sample positions.
pub(crate) struct Lcg(pub(crate) u64);
impl Lcg {
    pub(crate) fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

/// The shrunk copy: box averages over a grid of at most `budget` texels, the grid (sizes halved
/// from the file's) that keeps the most detail.
fn shrink(v: &Volume, budget: usize) -> ([usize; 3], Vec<f32>) {
    let halvings = |n: usize| -> Vec<usize> {
        let mut out = vec![n];
        let mut m = n;
        while m % 2 == 0 && m > 1 {
            m /= 2;
            out.push(m);
        }
        out
    };
    let mut best: Option<(f64, [usize; 3], Vec<f32>)> = None;
    for &x in &halvings(v.size[0]) {
        for &y in &halvings(v.size[1]) {
            for &z in &halvings(v.size[2]) {
                if x * y * z > budget {
                    continue;
                }
                let c = [x, y, z];
                let t = shrink_to(v, c);
                let err = shrink_error(v, c, &t);
                if best.as_ref().is_none_or(|b| err < b.0) {
                    best = Some((err, c, t));
                }
            }
        }
    }
    match best {
        Some((_, c, t)) => (c, t),
        None => {
            let c = [1, 1, 1];
            (c, shrink_to(v, c))
        }
    }
}

/// Mean squared error of the shrunk copy at sample texels.
fn shrink_error(v: &Volume, c: [usize; 3], t: &[f32]) -> f64 {
    let mut rng = Lcg(5);
    let mut sse = 0f64;
    for _ in 0..4096 {
        let p = [rng.next(v.size[0]), rng.next(v.size[1]), rng.next(v.size[2])];
        for k in 0..v.ch {
            sse += ((v.at(p[0], p[1], p[2], k) - shrunk_at(v, c, t, p, k)) as f64).powi(2);
        }
    }
    sse
}

/// The largest table a lookup-table stand-in may embed (`HZ_T3_BUDGET` overrides).
fn table_budget() -> usize {
    std::env::var("HZ_T3_BUDGET").ok().and_then(|s| s.parse().ok()).unwrap_or(256)
}

fn shrink_to(v: &Volume, c: [usize; 3]) -> Vec<f32> {
    let mut out = vec![0f32; c[0] * c[1] * c[2] * v.ch];
    for z in 0..c[2] {
        for y in 0..c[1] {
            for x in 0..c[0] {
                let r = |i: usize, n: usize, m: usize| (i * m / n)..((i + 1) * m / n).max(i * m / n + 1);
                let mut sum = vec![0f64; v.ch];
                let mut count = 0f64;
                for zz in r(z, c[2], v.size[2]) {
                    for yy in r(y, c[1], v.size[1]) {
                        for xx in r(x, c[0], v.size[0]) {
                            for (k, s) in sum.iter_mut().enumerate() {
                                *s += v.at(xx, yy, zz, k) as f64;
                            }
                            count += 1.0;
                        }
                    }
                }
                for k in 0..v.ch {
                    out[((z * c[1] + y) * c[0] + x) * v.ch + k] = (sum[k] / count) as f32;
                }
            }
        }
    }
    out
}

/// Linear filtering of the shrunk copy at texel `(x, y, z)` of the full size (clamped).
fn shrunk_at(v: &Volume, c: [usize; 3], t: &[f32], p: [usize; 3], k: usize) -> f32 {
    let mut i0 = [0usize; 3];
    let mut f = [0f32; 3];
    for a in 0..3 {
        let q = ((p[a] as f32 + 0.5) / v.size[a] as f32 * c[a] as f32 - 0.5).clamp(0.0, (c[a] - 1) as f32);
        i0[a] = q.floor() as usize;
        f[a] = q - q.floor();
    }
    let get = |dx: usize, dy: usize, dz: usize| {
        let x = (i0[0] + dx).min(c[0] - 1);
        let y = (i0[1] + dy).min(c[1] - 1);
        let z = (i0[2] + dz).min(c[2] - 1);
        t[((z * c[1] + y) * c[0] + x) * v.ch + k]
    };
    let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
    l(
        l(l(get(0, 0, 0), get(1, 0, 0), f[0]), l(get(0, 1, 0), get(1, 1, 0), f[0]), f[1]),
        l(l(get(0, 0, 1), get(1, 0, 1), f[0]), l(get(0, 1, 1), get(1, 1, 1), f[0]), f[1]),
        f[2],
    )
}

// The value noise, the same in Rust (to measure it) and GLSL (`NOISE_GLSL`).
fn fract(x: f32) -> f32 {
    x - x.floor()
}
fn hash(p: [f32; 3]) -> f32 {
    let mut q = [fract(p[0] * 0.1031), fract(p[1] * 0.1031), fract(p[2] * 0.1031)];
    let d = q[0] * (q[2] + 31.32) + q[1] * (q[1] + 31.32) + q[2] * (q[0] + 31.32);
    for v in &mut q {
        *v += d;
    }
    fract((q[0] + q[1]) * q[2])
}
pub(crate) fn value_noise(p: [f32; 3], n: f32, seed: f32) -> f32 {
    let q = [p[0] * n, p[1] * n, p[2] * n];
    let i = [q[0].floor(), q[1].floor(), q[2].floor()];
    let f: Vec<f32> = (0..3).map(|a| {
        let f = q[a] - i[a];
        f * f * (3.0 - 2.0 * f)
    }).collect();
    let mo = |x: f32| x - n * (x / n).floor();
    let h = |dx: f32, dy: f32, dz: f32| hash([mo(i[0] + dx) + seed, mo(i[1] + dy) + seed, mo(i[2] + dz) + seed]);
    let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
    l(
        l(l(h(0., 0., 0.), h(1., 0., 0.), f[0]), l(h(0., 1., 0.), h(1., 1., 0.), f[0]), f[1]),
        l(l(h(0., 0., 1.), h(1., 0., 1.), f[0]), l(h(0., 1., 1.), h(1., 1., 1.), f[0]), f[1]),
        f[2],
    )
}

pub(crate) const NOISE_GLSL: &str = "\
#ifndef HZ_T3_NOISE
#define HZ_T3_NOISE
float hz_t3_hash(vec3 p) { p = fract(p * 0.1031); p += dot(p, p.zyx + 31.32); return fract((p.x + p.y) * p.z); }
float hz_t3_noise(vec3 p, float n, float seed) {
    vec3 q = p * n; vec3 i = floor(q); vec3 f = q - i; f = f * f * (3.0 - 2.0 * f);
    vec3 a = i - n * floor(i / n) + seed; vec3 b = (i + 1.0) - n * floor((i + 1.0) / n) + seed;
    return mix(mix(mix(hz_t3_hash(vec3(a.x, a.y, a.z)), hz_t3_hash(vec3(b.x, a.y, a.z)), f.x),
                   mix(hz_t3_hash(vec3(a.x, b.y, a.z)), hz_t3_hash(vec3(b.x, b.y, a.z)), f.x), f.y),
               mix(mix(hz_t3_hash(vec3(a.x, a.y, b.z)), hz_t3_hash(vec3(b.x, a.y, b.z)), f.x),
                   mix(hz_t3_hash(vec3(a.x, b.y, b.z)), hz_t3_hash(vec3(b.x, b.y, b.z)), f.x), f.y), f.z);
}
#endif
";

/// Lag (in texels, along x) where a channel's correlation first drops below one half.
pub(crate) fn half_lag(get: &dyn Fn(usize, usize, usize) -> f32, size: [usize; 3]) -> f32 {
    let mut rng = Lcg(7);
    let pts: Vec<[usize; 3]> = (0..3000).map(|_| [rng.next(size[0]), rng.next(size[1]), rng.next(size[2])]).collect();
    let vals: Vec<f32> = pts.iter().map(|p| get(p[0], p[1], p[2])).collect();
    let mean = vals.iter().sum::<f32>() / vals.len() as f32;
    let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32;
    if var < 1e-9 {
        return size[0] as f32;
    }
    for lag in 1..size[0] / 2 {
        let cov = pts
            .iter()
            .zip(&vals)
            .map(|(p, v)| (v - mean) * (get((p[0] + lag) % size[0], p[1], p[2]) - mean))
            .sum::<f32>()
            / vals.len() as f32;
        if cov / var < 0.5 {
            return lag as f32;
        }
    }
    (size[0] / 2) as f32
}

pub(crate) fn mean_std(vals: &[f32]) -> (f32, f32) {
    let m = vals.iter().sum::<f32>() / vals.len() as f32;
    let v = vals.iter().map(|x| (x - m).powi(2)).sum::<f32>() / vals.len() as f32;
    (m, v.sqrt())
}

pub(crate) fn num(x: f32) -> String {
    let s = format!("{x:.6}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// GLSL for `vec4 hz_t3_<id>(vec3 p)` and `ivec3 hz_t3_size_<id>`; `how` says which kind.
fn volume_glsl(id: usize, v: &Volume) -> (String, String) {
    let pad = |c: &[String]| -> String {
        let mut c = c.to_vec();
        while c.len() < 3 {
            c.push("0.0".into());
        }
        if c.len() < 4 {
            c.push("1.0".into());
        }
        format!("vec4({})", c.join(", "))
    };
    let mut out = String::new();
    let (c, t) = shrink(v, table_budget());
    // How much of the data the shrunk copy explains.
    let mut rng = Lcg(1);
    let (mut sse, mut sst) = (0f64, 0f64);
    let means: Vec<f32> = (0..v.ch)
        .map(|k| (0..v.texels.len() / v.ch).map(|i| v.texels[i * v.ch + k]).sum::<f32>() / (v.texels.len() / v.ch) as f32)
        .collect();
    for _ in 0..4096 {
        let p = [rng.next(v.size[0]), rng.next(v.size[1]), rng.next(v.size[2])];
        for k in 0..v.ch {
            let a = v.at(p[0], p[1], p[2], k);
            sse += ((a - shrunk_at(v, c, &t, p, k)) as f64).powi(2);
            sst += ((a - means[k]) as f64).powi(2);
        }
    }
    let explained = if sst < 1e-9 { 1.0 } else { 1.0 - sse / sst };
    let how;
    if explained >= 0.75 {
        how = format!("lookup table, shrunk to {}x{}x{} ({:.0}% of its detail)", c[0], c[1], c[2], explained * 100.0);
        let n = c[0] * c[1] * c[2];
        let vals: Vec<String> = (0..n)
            .map(|i| pad(&(0..v.ch).map(|k| num(t[i * v.ch + k])).collect::<Vec<_>>()))
            .collect();
        let _ = writeln!(out, "const vec4 hz_t3_tab_{id}[{n}] = vec4[]({});", vals.join(", "));
        let _ = writeln!(
            out,
            "vec4 hz_t3_at_{id}(ivec3 c) {{ c = clamp(c, ivec3(0), ivec3({}, {}, {})); return hz_t3_tab_{id}[c.x + {} * (c.y + {} * c.z)]; }}",
            c[0] - 1, c[1] - 1, c[2] - 1, c[0], c[1]
        );
        let _ = writeln!(
            out,
            "vec4 hz_t3_{id}(vec3 p) {{\n    vec3 q = clamp(p, 0.0, 1.0) * vec3({}, {}, {}) - 0.5; vec3 i = floor(q); vec3 f = q - i; ivec3 b = ivec3(i);\n    \
             return mix(mix(mix(hz_t3_at_{id}(b), hz_t3_at_{id}(b + ivec3(1, 0, 0)), f.x), mix(hz_t3_at_{id}(b + ivec3(0, 1, 0)), hz_t3_at_{id}(b + ivec3(1, 1, 0)), f.x), f.y),\n               \
             mix(mix(hz_t3_at_{id}(b + ivec3(0, 0, 1)), hz_t3_at_{id}(b + ivec3(1, 0, 1)), f.x), mix(hz_t3_at_{id}(b + ivec3(0, 1, 1)), hz_t3_at_{id}(b + ivec3(1, 1, 1)), f.x), f.y), f.z);\n}}",
            c[0], c[1], c[2]
        );
    } else {
        // Noise: per channel, the cell count whose noise has the file's feature size.
        let mut chans = Vec::new();
        let mut desc = Vec::new();
        for k in 0..v.ch {
            let get = |x: usize, y: usize, z: usize| v.at(x, y, z, k);
            let target = half_lag(&get, v.size) / v.size[0] as f32; // as a fraction of the tile
            let seed = 13.0 + 71.0 * k as f32 + 191.0 * id as f32;
            // The noise's own half-lag is about 0.3 cells.
            let mut cells = (0.3 / target.max(1e-3)).round().clamp(1.0, (v.size[0] / 2) as f32);
            if cells < 1.0 {
                cells = 1.0;
            }
            let mut rng = Lcg(3 + k as u64);
            let vals: Vec<f32> = (0..v.texels.len() / v.ch).step_by((v.texels.len() / v.ch / 20000).max(1)).map(|i| v.texels[i * v.ch + k]).collect();
            let (dm, ds) = mean_std(&vals);
            let lo = vals.iter().cloned().fold(f32::MAX, f32::min);
            let hi = vals.iter().cloned().fold(f32::MIN, f32::max);
            let nv: Vec<f32> = (0..4000)
                .map(|_| {
                    let p = [rng.next(1000) as f32 / 1000.0, rng.next(1000) as f32 / 1000.0, rng.next(1000) as f32 / 1000.0];
                    value_noise(p, cells, seed)
                })
                .collect();
            let (nm, ns) = mean_std(&nv);
            let scale = if ns > 1e-6 { ds / ns } else { 0.0 };
            chans.push(format!(
                "clamp({} + (hz_t3_noise(p, {}, {}) - {}) * {}, {}, {})",
                num(dm), num(cells), num(seed), num(nm), num(scale), num(lo), num(hi)
            ));
            desc.push(format!("{cells}"));
        }
        how = format!("noise, {} cell(s) per tile by channel (the file's detail: {:.0}% in a shrunk copy)", desc.join("/"), explained.max(0.0) * 100.0);
        let _ = writeln!(out, "vec4 hz_t3_{id}(vec3 p) {{ return {}; }}", pad(&chans));
    }
    let _ = writeln!(out, "const ivec3 hz_t3_size_{id} = ivec3({}, {}, {});", v.size[0], v.size[1], v.size[2]);
    (out, how)
}

/// The stage a program file belongs to, as `texture.<stage>.*` names it.
pub(crate) fn stage_of(path: &str) -> &'static str {
    let stem = path.rsplit('/').next().unwrap_or(path);
    for s in ["gbuffers", "shadow", "deferred", "composite", "prepare", "begin", "setup", "dh"] {
        if stem.starts_with(s) {
            return match s {
                "dh" => "gbuffers",
                s => s,
            };
        }
    }
    if stem.starts_with("final") { "composite" } else { "gbuffers" }
}

/// `uniform sampler3D a, b;` statements: (start, end after `;`, names).
fn declarations(text: &str) -> Vec<(usize, usize, Vec<String>)> {
    let comments = comment_ranges(text);
    let mut out = Vec::new();
    for at in find_word(text, "sampler3D") {
        if in_comment(&comments, at) {
            continue;
        }
        let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
        let before = text[line_start..at].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.last() != Some(&"uniform") {
            continue;
        }
        let Some(semi) = text[at..].find(';').map(|i| at + i) else { continue };
        let names: Vec<String> = text[at + 9..semi]
            .split(',')
            .map(|n| n.trim().to_owned())
            .collect();
        if names.iter().any(|n| n.is_empty() || !n.bytes().all(is_ident_byte)) {
            continue;
        }
        let indent = line_start + (text[line_start..].len() - text[line_start..].trim_start().len());
        out.push((indent, semi + 1, names));
    }
    out
}

const MARK: &str = "// horizon-shaderport stand-in-3d";

pub fn stand_in_3d(files: &Files, _: &Eval) -> Edits {
    let srcs = sources(files);
    let mut names = BTreeSet::new();
    let mut edits = Edits::new();
    for (path, text) in &srcs {
        let decls = declarations(text);
        let comments = comment_ranges(text);
        let params: Vec<usize> = find_word(text, "sampler3D")
            .into_iter()
            .filter(|&at| !in_comment(&comments, at) && !decls.iter().any(|(s, e, _)| (*s..*e).contains(&at)))
            .collect();
        if decls.is_empty() && params.is_empty() {
            continue;
        }
        let mut out = text.clone();
        let mut hits: Vec<(usize, usize, String)> = Vec::new();
        for (s, e, ns) in &decls {
            let repl = ns
                .iter()
                .map(|n| format!("const hz_tex3d {n} = hz_tex3d(hz_t3id_{n});"))
                .collect::<Vec<_>>()
                .join(" ");
            names.extend(ns.iter().cloned());
            hits.push((*s, *e, repl));
        }
        for at in &params {
            hits.push((*at, at + 9, "hz_tex3d".into()));
        }
        hits.sort_by_key(|h| h.0);
        for (s, e, r) in hits.iter().rev() {
            out.replace_range(*s..*e, r);
        }
        edits.insert(path.clone(), (out, format!("{} declaration(s), {} parameter(s)", decls.len(), params.len())));
    }
    if edits.is_empty() {
        return edits;
    }
    // The data, once per file used.
    let entries = entries(files);
    let mut volumes: Vec<(Volume, String, String)> = Vec::new(); // volume, glsl, how
    let mut by_file: BTreeMap<String, Option<usize>> = BTreeMap::new();
    for (_, _, file, words) in &entries {
        if by_file.contains_key(file) {
            continue;
        }
        let id = read_volume(files, file, words).map(|v| {
            let id = volumes.len();
            let (glsl, how) = volume_glsl(id, &v);
            volumes.push((v, glsl, how));
            id
        });
        by_file.insert(file.clone(), id);
    }
    let lookup = |stage: &str, name: &str| -> Option<usize> {
        entries
            .iter()
            .filter(|(s, n, _, _)| n == name && (s == stage || s == "*"))
            .find_map(|(_, _, f, _)| by_file.get(f).copied().flatten())
    };
    let mut after = Files { name: files.name.clone(), entries: files.entries.clone(), layout_notes: Vec::new() };
    for (k, (t, _)) in &edits {
        after.set_text(k, t.clone());
    }
    for stage_file in stages_using(&after, "hz_tex3d") {
        let text = after.text(&stage_file).unwrap_or_default();
        if text.contains(MARK) {
            continue;
        }
        let stage = stage_of(&stage_file);
        let ids: BTreeMap<&String, Option<usize>> = names.iter().map(|n| (n, lookup(stage, n))).collect();
        let used: BTreeSet<usize> = ids.values().filter_map(|i| *i).collect();
        // Guarded: a program's file and the files it includes can all get one; the first (the
        // program's own, at its top) is the one with the right stage.
        let mut poly = format!(
            "{MARK}: 3D textures the game cannot bind, built from the pack's data\n#ifndef HZ_TEX3D\n#define HZ_TEX3D\nstruct hz_tex3d {{ int id; }};\n"
        );
        for (n, id) in &ids {
            let _ = writeln!(poly, "const int hz_t3id_{n} = {};", id.map_or(-1, |i| i as i64));
        }
        let noise = used.iter().any(|&i| volumes[i].1.contains("hz_t3_noise"));
        if noise {
            poly.push_str(NOISE_GLSL);
        }
        for &i in &used {
            let _ = writeln!(poly, "// {}: {}", volumes[i].0.file, volumes[i].2);
            poly.push_str(&volumes[i].1);
        }
        poly.push_str("vec4 hz_t3(int id, vec3 p) {\n");
        for &i in &used {
            let _ = writeln!(poly, "    if (id == {i}) return hz_t3_{i}(p);");
        }
        poly.push_str("    return vec4(0.0, 0.0, 0.0, 1.0);\n}\nivec3 hz_t3_size(int id) {\n");
        for &i in &used {
            let _ = writeln!(poly, "    if (id == {i}) return hz_t3_size_{i};");
        }
        poly.push_str(
            "    return ivec3(1);\n}\n\
             #ifndef texture\n\
             vec4 texture(hz_tex3d t, vec3 p) { return hz_t3(t.id, p); }\n\
             vec4 texture(hz_tex3d t, vec3 p, float bias) { return hz_t3(t.id, p); }\n\
             #endif\n\
             vec4 textureLod(hz_tex3d t, vec3 p, float lod) { return hz_t3(t.id, p); }\n\
             vec4 texelFetch(hz_tex3d t, ivec3 c, int lod) { return hz_t3(t.id, (vec3(c) + 0.5) / vec3(hz_t3_size(t.id))); }\n\
             ivec3 textureSize(hz_tex3d t, int lod) { return max(hz_t3_size(t.id) >> lod, ivec3(1)); }\n\
             #endif\n",
        );
        let entry = edits.entry(stage_file.clone()).or_insert_with(|| (text.clone(), String::new()));
        entry.0 = insert_decl(&entry.0, &poly);
        let kinds: Vec<String> = used.iter().map(|&i| format!("{} as {}", volumes[i].0.file, volumes[i].2)).collect();
        entry.1 = format!("{} stand-ins added ({})", entry.1, if kinds.is_empty() { "no data: reads 0".into() } else { kinds.join("; ") })
            .trim()
            .to_owned();
    }
    edits
}

/// Pass form of `stand-in-3d`: a 3D texture the converter's own load check accepts (the program
/// loads) is still refused by the game ("`sampler3D colortex6` is not provided yet": Mellow's sky),
/// so the stand-ins go in whenever nothing stops loading.
pub fn fix(files: &mut Files, log: &mut Vec<crate::shaderport::fix::Change>) {
    let Ok(best) = crate::shaderport::load::evaluate(files) else { return };
    let edits = stand_in_3d(files, &best);
    if edits.is_empty() {
        return;
    }
    let total = edits.len();
    let kept = crate::shaderport::pbr::keep(files, &best, edits, 0);
    println!("  stand-in-3d (pass): {} of {total} file(s) kept", kept.len());
    log.push(crate::shaderport::fix::Change {
        rule: "stand-in-3d",
        files: kept.into_iter().collect(),
        outcome: "3D textures the game does not provide read stand-ins built from the pack's data file (APPROXIMATE)".into(),
    });
}
