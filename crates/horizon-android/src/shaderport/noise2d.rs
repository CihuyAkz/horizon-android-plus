//! Pass `noise-2d`. The game loads no custom texture files (nightly 7aa0e28): `noisetex` is always
//! its own 256×256 white noise with alpha 1, whatever the pack sets with `texture.noise` or
//! `texture.<stage>.noisetex`. Packs that build clouds from their own smooth noise get the wrong
//! shapes (Photon: alpha is its cloud shape, so the sky is one solid sheet that draws no cloud and
//! everything sits in cloud shadow). `noisetex` becomes a stand-in built from the pack's image:
//!
//! - a smooth channel: tiling value noise at the feature size measured in the image, scaled to its
//!   average, contrast and range;
//! - a white-noise channel (neighbours unrelated): a hash per texel, scaled the same way;
//! - a flat channel: its value.
//!
//! A stage with no image of its own reads white noise with alpha 1, like the game's. APPROXIMATE:
//! the shapes differ from the pack's; the amount and scale are close.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Change, Edits, insert_decl, sources, stages_using};
use crate::shaderport::glsl::{comment_ranges, find_word, in_comment};
use crate::shaderport::tex3d::{Lcg, NOISE_GLSL, half_lag, mean_std, num, stage_of, value_noise};

const MARK: &str = "// horizon-shaderport noise-2d";

pub(crate) struct Image {
    pub(crate) file: String,
    pub(crate) size: [usize; 2],
    pub(crate) texels: Vec<[f32; 4]>,
}

/// `texture.noise` (every stage, `*`) and `texture.<stage>.noisetex`: stage → file.
fn entries(files: &Files) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(props) = files.text("shaders.properties") else { return out };
    for line in props.lines() {
        let Some((key, value)) = line.trim().split_once('=') else { continue };
        let file = value.trim();
        if file.is_empty() || file.contains(':') || file.split_whitespace().count() != 1 {
            continue;
        }
        let stage = match key.trim().split('.').collect::<Vec<_>>().as_slice() {
            ["texture", "noise"] => "*".to_owned(),
            ["texture", stage, "noisetex"] => stage.to_string(),
            _ => continue,
        };
        out.entry(stage).or_insert_with(|| file.to_owned());
    }
    out
}

pub(crate) fn read_png(files: &Files, file: &str) -> Option<Image> {
    let data = files.entries.get(&format!("shaders/{}", file.trim_start_matches('/')))?;
    let mut dec = png::Decoder::new(std::io::Cursor::new(data.as_slice()));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let ch = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return None,
    };
    let mut texels = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = &buf[y * info.line_size..];
        for x in 0..w {
            let t: Vec<f32> = row[x * ch..x * ch + ch].iter().map(|&b| b as f32 / 255.0).collect();
            texels.push(match ch {
                1 => [t[0], t[0], t[0], 1.0],
                2 => [t[0], t[0], t[0], t[1]],
                3 => [t[0], t[1], t[2], 1.0],
                _ => [t[0], t[1], t[2], t[3]],
            });
        }
    }
    Some(Image { file: file.to_owned(), size: [w, h], texels })
}

/// When channel `k` is channel `j` moved by whole texels (`k(p) == j(p + off)` everywhere, wrapping),
/// `off` in texture coordinates (GL's: rows count up from the bottom). Not for a shift of 0 with
/// flat channels, which are handled as flat.
fn shift_of(img: &Image, j: usize, k: usize) -> Option<[f32; 2]> {
    let [w, h] = img.size;
    let at = |c: usize, x: usize, y: usize| img.texels[(y % h) * w + (x % w)][c];
    let tol = 0.6 / 255.0;
    if img.texels.iter().all(|t| (t[k] - img.texels[0][k]).abs() <= tol) {
        return None;
    }
    let mut rng = Lcg(3);
    let pts: Vec<(usize, usize)> = (0..64).map(|_| (rng.next(w), rng.next(h))).collect();
    // An image that repeats itself (Bliss's, moved by half its width) matches at several shifts;
    // the stand-in does not repeat, so the shortest is taken: packs offset by small steps.
    let mut best: Option<(usize, usize, usize)> = None;
    for sy in 0..h {
        for sx in 0..w {
            if !pts.iter().all(|&(x, y)| (at(k, x, y) - at(j, x + sx, y + sy)).abs() <= tol) {
                continue;
            }
            // Confirm on every texel.
            if (0..h).all(|y| (0..w).all(|x| (at(k, x, y) - at(j, x + sx, y + sy)).abs() <= tol)) {
                let len = sx.min(w - sx) + sy.min(h - sy);
                if best.is_none_or(|b| len < b.0) {
                    best = Some((len, sx, sy));
                }
            }
        }
    }
    // Wrapped to the nearest; image row y + sy is GL row (h - 1 - y) - sy.
    let wrap = |s: usize, n: usize| if s * 2 > n { s as f32 - n as f32 } else { s as f32 };
    best.map(|(_, sx, sy)| [wrap(sx, w) / w as f32, -wrap(sy, h) / h as f32])
}

/// GLSL `vec4 hz_n2_img(vec2 p)` and a description.
fn image_glsl(img: &Image) -> (String, String) {
    let [w, h] = img.size;
    let mut chans = Vec::new();
    let mut desc = Vec::new();
    let mut funcs = String::new();
    // Per channel, its expression with `{P}` for the coordinate (to reuse it shifted).
    let mut exprs: Vec<Option<String>> = vec![None; 4];
    for k in 0..4 {
        if let Some((j, off)) = (0..k).find_map(|j| exprs[j].as_ref().and_then(|_| shift_of(img, j, k)).map(|o| (j, o))) {
            // A copy of an earlier channel moved by whole texels (Bliss: green is red 193 rows
            // on, so one fetch reads two layers of its 3D noise): the same noise, moved.
            let p = format!("(p + vec2({}, {}))", num(off[0]), num(off[1]));
            chans.push(exprs[j].as_ref().unwrap().replace("{P}", &p));
            desc.push(format!("{} = {} moved ({}, {}) texels", ["r", "g", "b", "a"][k], ["r", "g", "b", "a"][j],
                (off[0] * w as f32).round(), (off[1] * h as f32).round()));
            continue;
        }
        // Image rows are y; GL's texture origin is the bottom row. Measured along x only.
        let get = |x: usize, y: usize, _: usize| img.texels[y * w + x][k];
        let vals: Vec<f32> = img.texels.iter().step_by((w * h / 20000).max(1)).map(|t| t[k]).collect();
        let (dm, ds) = mean_std(&vals);
        let lo = vals.iter().cloned().fold(f32::MAX, f32::min);
        let hi = vals.iter().cloned().fold(f32::MIN, f32::max);
        let seed = 17.0 + 53.0 * k as f32;
        if ds < 1e-4 {
            chans.push(num(dm));
            desc.push(format!("{}", ["r", "g", "b", "a"][k].to_owned() + " flat"));
            continue;
        }
        let lag = half_lag(&get, [w, h, 1]);
        if lag <= 1.0 {
            // Uniform hash: mean 0.5, std 1/sqrt(12).
            let e = format!(
                "clamp({} + (hz_t3_hash(vec3(mod(floor({{P}} * vec2({w}.0, {h}.0)), vec2({w}.0, {h}.0)), {})) - 0.5) * {}, {}, {})",
                num(dm), num(seed), num(ds / 0.288_675), num(lo), num(hi)
            );
            chans.push(e.replace("{P}", "p"));
            exprs[k] = Some(e);
            desc.push(format!("{} white", ["r", "g", "b", "a"][k]));
            continue;
        }
        let cells = (0.3 / (lag / w as f32)).round().clamp(1.0, (w / 2) as f32);
        let mut rng = Lcg(11 + k as u64);
        let mut nv: Vec<f32> = (0..20000)
            .map(|_| value_noise([rng.next(10000) as f32 / 10000.0, rng.next(10000) as f32 / 10000.0, 0.0], cells, seed))
            .collect();
        // The noise goes through the image's own distribution (17 matching quantiles), so the
        // tails match too: packs cut clouds out of the top tenth (Photon's coverage).
        nv.sort_by(|a, b| a.total_cmp(b));
        let mut iv: Vec<f32> = img.texels.iter().map(|t| t[k]).collect();
        iv.sort_by(|a, b| a.total_cmp(b));
        let q = |v: &[f32], f: f32| v[((v.len() - 1) as f32 * f).round() as usize];
        let knots: Vec<(f32, f32)> = (0..=16).map(|i| (q(&nv, i as f32 / 16.0), q(&iv, i as f32 / 16.0))).collect();
        let fname = format!("hz_n2_q{k}");
        let mut f = format!("float {fname}(float x) {{\n");
        for w in knots.windows(2) {
            let ((x0, y0), (x1, y1)) = (w[0], w[1]);
            let _ = writeln!(
                f,
                "    if (x <= {}) return mix({}, {}, clamp((x - {}) / {}, 0.0, 1.0));",
                num(x1), num(y0), num(y1), num(x0), num((x1 - x0).max(1e-6))
            );
        }
        let _ = writeln!(f, "    return {};\n}}", num(knots[16].1));
        funcs.push_str(&f);
        let e = format!("{fname}(hz_t3_noise(vec3({{P}}, 0.0), {}, {}))", num(cells), num(seed));
        chans.push(e.replace("{P}", "p"));
        exprs[k] = Some(e);
        desc.push(format!("{} smooth ({cells} cells)", ["r", "g", "b", "a"][k]));
    }
    let glsl = format!(
        "{funcs}vec4 hz_n2_img(vec2 p) {{ return vec4({}); }}\nconst ivec2 hz_n2_size = ivec2({w}, {h});\n",
        chans.join(", ")
    );
    (glsl, format!("{} {w}x{h}: {}", img.file, desc.join(", ")))
}

/// For a stage with no image: white noise with alpha 1, 256×256 (the game's own).
const DEFAULT_GLSL: &str = "\
vec4 hz_n2_img(vec2 p) { vec2 c = mod(floor(p * 256.0), 256.0); return vec4(hz_t3_hash(vec3(c, 17.0)), hz_t3_hash(vec3(c, 70.0)), hz_t3_hash(vec3(c, 123.0)), 1.0); }
const ivec2 hz_n2_size = ivec2(256);
";

const OVERLOADS: &str = "\
#ifndef texture
vec4 texture(bvec3 t, vec2 p) { return hz_n2_img(p); }
vec4 texture(bvec3 t, vec2 p, float bias) { return hz_n2_img(p); }
#endif
vec4 textureLod(bvec3 t, vec2 p, float lod) { return hz_n2_img(p); }
vec4 textureGrad(bvec3 t, vec2 p, vec2 dx, vec2 dy) { return hz_n2_img(p); }
vec4 texelFetch(bvec3 t, ivec2 c, int lod) { return hz_n2_img((vec2(c) + 0.5) / vec2(hz_n2_size)); }
ivec2 textureSize(bvec3 t, int lod) { return max(hz_n2_size >> lod, ivec2(1)); }
#endif
";

/// The stand-in's own overloads and GLSL's texture functions: no copy needed.
const BUILTIN: [&str; 14] = [
    "texture", "textureLod", "textureGrad", "texelFetch", "textureSize", "texture2D", "texture2DLod",
    "texelFetch2D", "textureGather", "textureOffset", "textureLodOffset", "texture2DGradARB", "textureProj",
    "textureQueryLod",
];

/// Functions called with `arg` as their first argument (not GLSL's own), added to `out`.
fn callers_of(text: &str, arg: &str, out: &mut Vec<String>) {
    let comments = comment_ranges(text);
    for at in find_word(text, arg) {
        if in_comment(&comments, at) {
            continue;
        }
        let before = text[..at].trim_end();
        let Some(b) = before.strip_suffix('(') else { continue };
        let b = b.trim_end();
        let start = b.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(0, |i| i + 1);
        let name = &b[start..];
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || BUILTIN.contains(&name) {
            continue;
        }
        if !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
}

/// Copies each definition `<type> name(sampler2D p, ...) { ... }` in `text` with `bvec3 p`,
/// under `#ifdef HZ_TEX2N`. Returns the new text and the copied parameters' names.
fn copy_helper(text: &str, name: &str) -> Option<(String, Vec<String>)> {
    if text.contains(&format!("// hz-n2 copy of {name}\n")) {
        return None;
    }
    let comments = comment_ranges(text);
    let mut out = text.to_owned();
    let mut params = Vec::new();
    let mut inserts = Vec::new();
    for at in find_word(text, name) {
        if in_comment(&comments, at) {
            continue;
        }
        // A definition: a type word before, `(sampler2D p` after, then `) {`.
        let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
        let head: Vec<&str> = text[ls..at].split_whitespace().collect();
        if head.len() != 1 {
            continue;
        }
        let rest = &text[at + name.len()..];
        let Some(r) = rest.trim_start().strip_prefix('(') else { continue };
        let r = r.trim_start();
        let Some(r) = r.strip_prefix("sampler2D") else { continue };
        let pname: String = r.trim_start().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if pname.is_empty() {
            continue;
        }
        let Some(open) = rest.find('{').map(|i| at + name.len() + i) else { continue };
        if text[at..open].contains(';') {
            continue; // a prototype
        }
        let mut depth = 0;
        let mut close = None;
        for (i, c) in text[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close?;
        let def = &text[ls..close];
        let copy = def.replacen("sampler2D", "bvec3", 1);
        inserts.push((close, format!("\n#ifdef HZ_TEX2N\n// hz-n2 copy of {name}\n{copy}\n#endif\n")));
        if !params.contains(&pname) {
            params.push(pname);
        }
    }
    if inserts.is_empty() {
        return None;
    }
    for (at, s) in inserts.into_iter().rev() {
        out.insert_str(at, &s);
    }
    Some((out, params))
}

fn edits(files: &Files) -> (Edits, Vec<String>) {
    let entries = entries(files);
    if entries.is_empty() {
        return (Edits::new(), Vec::new());
    }
    let mut images: BTreeMap<String, Option<(String, String)>> = BTreeMap::new();
    for file in entries.values() {
        images.entry(file.clone()).or_insert_with(|| read_png(files, file).map(|i| image_glsl(&i)));
    }
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let comments = comment_ranges(&text);
        let mut hits = Vec::new();
        for at in find_word(&text, "noisetex") {
            if in_comment(&comments, at) {
                continue;
            }
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let before: Vec<&str> = text[line_start..at].split_whitespace().collect();
            if before != ["uniform", "sampler2D"] {
                continue;
            }
            let Some(semi) = text[at..].find(';').map(|i| at + i) else { continue };
            if !text[at + "noisetex".len()..semi].trim().is_empty() {
                continue;
            }
            let indent = line_start + (text[line_start..].len() - text[line_start..].trim_start().len());
            hits.push((indent, semi + 1));
        }
        if hits.is_empty() {
            continue;
        }
        let mut out = text.clone();
        for (s, e) in hits.iter().rev() {
            out.replace_range(*s..*e, "const bvec3 noisetex = bvec3(true);");
        }
        edits.insert(path, (out, "noisetex stand-in".into()));
    }
    if edits.is_empty() {
        return (edits, Vec::new());
    }
    // The pack's own helpers handed `noisetex` (Noble's `textureBicubic(sampler2D, vec2)`) get a
    // copy taking the stand-in; without it the declaration's file breaks a program and the whole
    // stand-in is dropped (Noble's clouds then read the game's white noise).
    let srcs = sources(files);
    let mut wanted: Vec<String> = Vec::new();
    for (_, text) in &srcs {
        callers_of(text, "noisetex", &mut wanted);
    }
    let mut seen = 0;
    while seen < wanted.len() {
        let name = wanted[seen].clone();
        seen += 1;
        for (path, text) in &srcs {
            let base = edits.get(path).map_or(text.clone(), |e| e.0.clone());
            if let Some((out, params)) = copy_helper(&base, &name) {
                for p in params {
                    callers_of(&out, &p, &mut wanted);
                }
                edits.insert(path.clone(), (out, format!("noisetex stand-in: `{name}` copy")));
            }
        }
    }
    let mut after = Files { name: files.name.clone(), entries: files.entries.clone(), layout_notes: Vec::new() };
    for (k, (t, _)) in &edits {
        after.set_text(k, t.clone());
    }
    let mut used = Vec::new();
    for stage_file in stages_using(&after, "const bvec3 noisetex") {
        let text = after.text(&stage_file).unwrap_or_default();
        if text.contains(MARK) {
            continue;
        }
        let stage = stage_of(&stage_file);
        let file = entries.get(stage).or_else(|| entries.get("*"));
        let found = file.and_then(|f| images.get(f).cloned().flatten());
        let mut poly = format!("{MARK}: the game loads no noise texture; built from the pack's image\n#ifndef HZ_TEX2N\n#define HZ_TEX2N\n// noisetex is a bvec3: no other stand-in overloads that type (naga takes two struct types as one)\n");
        poly.push_str(NOISE_GLSL);
        match &found {
            Some((glsl, how)) => {
                let _ = writeln!(poly, "// {how}");
                poly.push_str(glsl);
                if !used.contains(how) {
                    used.push(how.clone());
                }
            }
            None => poly.push_str(DEFAULT_GLSL),
        }
        poly.push_str(OVERLOADS);
        let entry = edits.entry(stage_file.clone()).or_insert_with(|| (text.clone(), String::new()));
        entry.0 = insert_decl(&entry.0, &poly);
    }
    (edits, used)
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let (edits, used) = edits(files);
    if edits.is_empty() || used.is_empty() {
        return;
    }
    let Ok(best) = crate::shaderport::load::evaluate(files) else { return };
    let total = edits.len();
    if std::env::var_os("HZ_TRACE").is_some() {
        let mut trial = Files { name: files.name.clone(), entries: files.entries.clone(), layout_notes: Vec::new() };
        for (k, (t, _)) in &edits {
            trial.set_text(k, t.clone());
        }
        if let Ok(e) = crate::shaderport::load::evaluate(&trial) {
            for d in &e.dims {
                for s in d.skipped.iter().take(3) {
                    for p in s.problems.iter().take(2) {
                        eprintln!("  [trace] noise-2d {}: {}:{}: {}", s.program, p.file, p.line, p.message);
                    }
                }
            }
        }
    }
    let kept = crate::shaderport::pbr::keep(files, &best, edits, 0);
    println!("  noise-2d: {} of {total} file(s) kept ({})", kept.len(), used.join("; "));
    log.push(Change {
        rule: "noise-2d",
        files: kept.into_iter().collect(),
        outcome: format!("`noisetex` reads a stand-in built from the pack's own noise image ({}) (APPROXIMATE)", used.join("; ")),
    });
}
