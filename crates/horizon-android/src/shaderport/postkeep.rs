//! Pass `post-keep`. Horizon starts every deferred/composite pass on a cleared target ("overwritten
//! whole", `LoadOp::Clear`); Iris and OptiFine keep what the target held. Two things break:
//!
//! - a pass that draws only part of the screen (Noble's bloom draws each tile into one corner)
//!   wipes everything else;
//! - a pass with `blend.<program>` blends into zero instead of into the buffer.
//!
//! Noble's bloom tiles came out wrong, its auto-exposure read them, and the whole picture was
//! 330 times too bright. The fix, in the shaders:
//!
//! - a partial pass is drawn over the whole screen: the vertex stage runs the pack's `main` at
//!   three corners to find where the pack's quad lands, then again at the inverse-mapped corner
//!   so every output varies as before; the fragment stage copies the buffer's current texel
//!   through outside the quad;
//! - a blended pass reads the buffer's current texel and blends with it itself, and its
//!   `blend.*` lines become `off`.

use std::collections::{BTreeMap, BTreeSet};

use horizon_shaders::{Factor, Group, Macros, Properties, dimension_folder, load_post, preprocess};

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Change, Edits};
use crate::shaderport::glsl::{after_version, comment_ranges, find_word, in_comment};

const MARK: &str = "// horizon-shaderport post-keep";

/// Old names of colortex0-7 (`gaux1` is colortex4).
const ALIASES: [&str; 8] = ["gcolor", "gdepth", "gnormal", "composite", "gaux1", "gaux2", "gaux3", "gaux4"];

struct Output {
    location: u32,
    ty: String,
    name: String,
}

/// `layout (location = N) out TYPE NAME;` declarations of preprocessed code.
fn outputs(code: &str) -> Vec<Output> {
    let comments = comment_ranges(code);
    let mut v = Vec::new();
    for at in find_word(code, "layout") {
        if in_comment(&comments, at) {
            continue;
        }
        let Some(semi) = code[at..].find(';').map(|i| at + i) else { continue };
        let decl = &code[at..semi];
        let Some(close) = decl.find(')') else { continue };
        let inside = &decl[decl.find('(').unwrap_or(0) + 1..close];
        let Some(loc) = inside.split(',').find_map(|p| {
            let (k, val) = p.split_once('=')?;
            (k.trim() == "location").then(|| val.trim().parse::<u32>().ok()).flatten()
        }) else {
            continue;
        };
        let words: Vec<&str> = decl[close + 1..].split_whitespace().collect();
        if let ["out", ty, name] = words[..] {
            v.push(Output { location: loc, ty: ty.to_owned(), name: name.to_owned() });
        }
    }
    v
}

/// The sampler the code declares for buffer `b`, or `None` when it declares none (it gets
/// `colortex<b>`); `Err` when it declares it with another type (integer buffers).
fn sampler(code: &str, b: u32) -> Result<Option<String>, ()> {
    let mut names = vec![format!("colortex{b}")];
    if let Some(a) = ALIASES.get(b as usize) {
        names.push((*a).to_owned());
    }
    for n in names {
        for at in find_word(code, &n) {
            let line_start = code[..at].rfind(|c| c == ';' || c == '\n' || c == '}').map_or(0, |i| i + 1);
            let before: Vec<&str> = code[line_start..at].split_whitespace().collect();
            if before.last() == Some(&"sampler2D") && before.contains(&"uniform") {
                return Ok(Some(n));
            }
            if before.last().is_some_and(|w| w.ends_with("sampler2D")) {
                return Err(());
            }
        }
    }
    Ok(None)
}

fn factor(f: Factor, rgb: bool) -> &'static str {
    match (f, rgb) {
        (Factor::Zero, _) => "0.0",
        (Factor::One, _) => "1.0",
        (Factor::SrcColor, true) => "hz_s.rgb",
        (Factor::SrcColor, false) => "hz_s.a",
        (Factor::OneMinusSrcColor, true) => "(1.0 - hz_s.rgb)",
        (Factor::OneMinusSrcColor, false) => "(1.0 - hz_s.a)",
        (Factor::DstColor, true) => "hz_d.rgb",
        (Factor::DstColor, false) => "hz_d.a",
        (Factor::OneMinusDstColor, true) => "(1.0 - hz_d.rgb)",
        (Factor::OneMinusDstColor, false) => "(1.0 - hz_d.a)",
        (Factor::SrcAlpha, _) => "hz_s.a",
        (Factor::OneMinusSrcAlpha, _) => "(1.0 - hz_s.a)",
        (Factor::DstAlpha, _) => "hz_d.a",
        (Factor::OneMinusDstAlpha, _) => "(1.0 - hz_d.a)",
        (Factor::SrcAlphaSaturate, true) => "min(hz_s.a, 1.0 - hz_d.a)",
        (Factor::SrcAlphaSaturate, false) => "1.0",
    }
}

fn parse_blend(value: &str) -> Option<Option<[Factor; 4]>> {
    let value = value.trim();
    if value == "off" {
        return Some(None);
    }
    let f: Vec<Factor> = value
        .split_whitespace()
        .map(|w| {
            Some(match w {
                "ZERO" => Factor::Zero,
                "ONE" => Factor::One,
                "SRC_COLOR" => Factor::SrcColor,
                "ONE_MINUS_SRC_COLOR" => Factor::OneMinusSrcColor,
                "DST_COLOR" => Factor::DstColor,
                "ONE_MINUS_DST_COLOR" => Factor::OneMinusDstColor,
                "SRC_ALPHA" => Factor::SrcAlpha,
                "ONE_MINUS_SRC_ALPHA" => Factor::OneMinusSrcAlpha,
                "DST_ALPHA" => Factor::DstAlpha,
                "ONE_MINUS_DST_ALPHA" => Factor::OneMinusDstAlpha,
                "SRC_ALPHA_SATURATE" => Factor::SrcAlphaSaturate,
                _ => return None,
            })
        })
        .collect::<Option<_>>()?;
    Some(Some(f.try_into().ok()?))
}

/// Whether every `gl_Position = ...;` of the vertex code is a plain full-screen quad.
fn full_screen(code: &str) -> bool {
    let comments = comment_ranges(code);
    let mut any = false;
    for at in find_word(code, "gl_Position") {
        if in_comment(&comments, at) {
            continue;
        }
        let rest = code[at + 11..].trim_start();
        if !rest.starts_with('=') || rest.starts_with("==") {
            // A component write (`gl_Position.xy = ...`) moves the quad.
            if rest.starts_with('.') {
                return false;
            }
            continue;
        }
        let Some(end) = rest.find(';') else { return false };
        let rhs: String = rest[1..end].chars().filter(|c| !c.is_whitespace()).collect();
        let rhs = rhs.replace(".0", "").replace("0.", "0");
        any = true;
        let ok = matches!(
            rhs.as_str(),
            "ftransform()"
                | "gl_ModelViewProjectionMatrix*gl_Vertex"
                | "gl_ProjectionMatrix*gl_ModelViewMatrix*gl_Vertex"
                | "vec4(gl_Vertex.xy*2-1,0,1)"
                | "vec4(gl_Vertex.xy*2-1,1,1)"
                | "vec4(gl_Vertex.xy*2-1,gl_Vertex.zw)"
                | "vec4(gl_Vertex.xy*2-1,-1,1)"
        );
        if !ok {
            return false;
        }
    }
    any
}

const VERTEX: &str = "
#undef main
// horizon-shaderport post-keep: the pass covers the whole screen; the pack's quad is found from
// three corners and every output is computed at the matching point of it.
flat out vec2 hz_keep_p0;
flat out vec2 hz_keep_e1;
flat out vec2 hz_keep_e2;
vec2 hz_keep_ndc(vec2 v) {
    gl_Vertex = vec4(v, 0.0, 1.0);
    gl_MultiTexCoord0 = gl_Vertex;
    hz_keep_vmain();
    return gl_Position.xy / gl_Position.w;
}
void main() {
    vec4 hz_corner = gl_Vertex;
    vec2 p0 = hz_keep_ndc(vec2(0.0));
    vec2 e1 = hz_keep_ndc(vec2(1.0, 0.0)) - p0;
    vec2 e2 = hz_keep_ndc(vec2(0.0, 1.0)) - p0;
    vec2 c = hz_corner.xy * 2.0 - 1.0;
    mat2 m = mat2(e1, e2);
    vec2 v = abs(determinant(m)) > 1e-8 ? inverse(m) * (c - p0) : hz_corner.xy;
    gl_Vertex = vec4(v, hz_corner.zw);
    gl_MultiTexCoord0 = gl_Vertex;
    hz_keep_vmain();
    gl_Position = vec4(c, gl_Position.z / gl_Position.w, 1.0);
    hz_keep_p0 = p0;
    hz_keep_e1 = e1;
    hz_keep_e2 = e2;
}
";

fn swizzle(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "float" => ".r",
        "vec2" => ".rg",
        "vec3" => ".rgb",
        "vec4" => "",
        _ => return None,
    })
}

struct Plan {
    fsh: String,
    vsh: Option<String>,
    blended: bool,
}

/// The edits for one program, or why not.
fn plan(
    files: &Files,
    pack: &horizon_shaders::Pack,
    stem: &str,
    buffers: &[u32],
    blends: &BTreeMap<u32, [Factor; 4]>,
    partial: bool,
) -> Result<Plan, String> {
    let macros = Macros::standard();
    let fpath = format!("{stem}.fsh");
    let ftext = files.text(&fpath).ok_or("no fragment file")?;
    if ftext.contains(MARK) {
        return Err("already done".into());
    }
    let code = preprocess(pack, &fpath, &macros).code;
    let outs = outputs(&code);
    if outs.is_empty() {
        return Err("no `layout(location = N) out` declarations".into());
    }
    let mut decls = String::new();
    let mut copy = String::new();
    let mut blend = String::new();
    let mut size_of = None;
    for o in &outs {
        let Some(&b) = buffers.get(o.location as usize) else { continue };
        let sw = swizzle(&o.ty).ok_or(format!("output `{}` is {}", o.name, o.ty))?;
        let s = match sampler(&code, b) {
            Ok(Some(n)) => n,
            Ok(None) => {
                let n = format!("colortex{b}");
                if !decls.contains(&n) {
                    decls.push_str(&format!("uniform sampler2D {n};\n"));
                }
                n
            }
            Err(()) => return Err(format!("buffer {b} is read as an integer texture")),
        };
        size_of.get_or_insert(s.clone());
        copy.push_str(&format!("        {} = texelFetch({s}, hz_px, 0){sw};\n", o.name));
        if let Some(f) = blends.get(&b) {
            let src = match o.ty.as_str() {
                "float" => format!("vec4({}, 0.0, 0.0, 1.0)", o.name),
                "vec2" => format!("vec4({}, 0.0, 1.0)", o.name),
                "vec3" => format!("vec4({}, 1.0)", o.name),
                _ => o.name.clone(),
            };
            blend.push_str(&format!(
                "    {{\n        vec4 hz_s = {src};\n        vec4 hz_d = texelFetch({s}, hz_px, 0);\n        \
                 vec4 hz_r = vec4(hz_s.rgb * {} + hz_d.rgb * {}, hz_s.a * {} + hz_d.a * {});\n        {} = hz_r{sw};\n    }}\n",
                factor(f[0], true),
                factor(f[1], true),
                factor(f[2], false),
                factor(f[3], false),
                o.name
            ));
        }
    }
    let size_of = size_of.ok_or("writes no buffer")?;
    let mut tail = format!("\n#undef main\n{MARK}\n{decls}");
    if partial {
        tail.push_str("flat in vec2 hz_keep_p0;\nflat in vec2 hz_keep_e1;\nflat in vec2 hz_keep_e2;\n");
    }
    tail.push_str("void main() {\n    ivec2 hz_px = ivec2(gl_FragCoord.xy);\n");
    if partial {
        tail.push_str(&format!(
            "    vec2 hz_ndc = gl_FragCoord.xy / vec2(textureSize({size_of}, 0)) * 2.0 - 1.0;\n    \
             vec2 hz_v = inverse(mat2(hz_keep_e1, hz_keep_e2)) * (hz_ndc - hz_keep_p0);\n    \
             if (any(lessThan(hz_v, vec2(0.0))) || any(greaterThan(hz_v, vec2(1.0)))) {{\n{copy}        return;\n    }}\n"
        ));
    }
    tail.push_str("    hz_keep_main();\n");
    tail.push_str(&blend);
    tail.push_str("}\n");
    let at = after_version(&ftext).ok_or("no #version")?;
    let fsh = format!("{}#define main hz_keep_main\n{}{tail}", &ftext[..at], &ftext[at..]);
    let vsh = if partial {
        let vpath = format!("{stem}.vsh");
        let vtext = files.text(&vpath).ok_or("no vertex file")?;
        let at = after_version(&vtext).ok_or("no #version in the vertex file")?;
        Some(format!("{}#define main hz_keep_vmain\n{}\n{VERTEX}", &vtext[..at], &vtext[at..]))
    } else {
        None
    };
    Ok(Plan { fsh, vsh, blended: !blend.is_empty() })
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let Ok(pack) = files.pack() else { return };
    let macros = Macros::standard();
    let ptext = files.text("shaders.properties").unwrap_or_default();
    let properties = Properties::parse(&ptext, &macros);
    let inputs = crate::shaderport::load::inputs();
    let trace = std::env::var_os("HZ_TRACE").is_some();
    let mut edits = Edits::new();
    let mut unblend: BTreeSet<String> = BTreeSet::new();
    let mut done = BTreeSet::new();
    for id in [0, -1, 1] {
        let folder = dimension_folder(&pack, id);
        let Ok(post) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_post(&pack, &folder, &properties, &macros, &inputs)
        })) else {
            continue;
        };
        for p in &post.programs {
            if !matches!(p.group, Group::Prepare | Group::Deferred | Group::Composite) {
                continue;
            }
            let stem = [format!("{folder}/{}", p.name), p.name.clone()]
                .into_iter()
                .find(|s| !s.starts_with('/') && files.text(&format!("{s}.fsh")).is_some());
            let Some(stem) = stem else { continue };
            if !done.insert(stem.clone()) {
                continue;
            }
            let mut blends = BTreeMap::new();
            let whole = properties.get(&format!("blend.{}", p.name)).and_then(parse_blend).flatten();
            for &b in &p.buffers {
                let own = properties
                    .get(&format!("blend.{}.colortex{b}", p.name))
                    .and_then(parse_blend);
                if let Some(f) = own.unwrap_or(whole) {
                    blends.insert(b, f);
                }
            }
            let vcode = files
                .text(&format!("{stem}.vsh"))
                .map(|_| preprocess(&pack, &format!("{stem}.vsh"), &macros).code);
            let partial = vcode.as_deref().is_some_and(|c| !full_screen(c));
            if blends.is_empty() && !partial {
                continue;
            }
            match plan(files, &pack, &stem, &p.buffers, &blends, partial) {
                Ok(plan) => {
                    let what = match (partial, plan.blended) {
                        (true, true) => "partial, blended",
                        (true, false) => "partial",
                        _ => "blended",
                    };
                    edits.insert(format!("{stem}.fsh"), (plan.fsh, what.into()));
                    if let Some(v) = plan.vsh {
                        edits.insert(format!("{stem}.vsh"), (v, what.into()));
                    }
                    if plan.blended {
                        unblend.insert(p.name.clone());
                    }
                }
                Err(why) => {
                    if trace {
                        eprintln!("  [trace] post-keep {stem}: {why}");
                    }
                }
            }
        }
    }
    if edits.is_empty() {
        return;
    }
    if !unblend.is_empty() {
        let lines: Vec<String> = ptext
            .split('\n')
            .map(|l| {
                let t = l.trim_start();
                let Some(rest) = t.strip_prefix("blend.") else { return l.to_owned() };
                let Some((key, _)) = rest.split_once('=') else { return l.to_owned() };
                let prog = key.trim().split('.').next().unwrap_or("");
                if unblend.contains(prog) {
                    format!("{}= off", &l[..l.len() - t.len() + 6 + key.len()])
                } else {
                    l.to_owned()
                }
            })
            .collect();
        edits.insert("shaders.properties".into(), (lines.join("\n"), String::new()));
    }
    let Ok(best) = crate::shaderport::load::evaluate(files) else { return };
    let total = edits.keys().filter(|k| k.ends_with(".fsh")).count();
    let kept = crate::shaderport::pbr::keep(files, &best, edits, 0);
    println!("  post-keep: {} file(s) kept ({total} program(s) planned)", kept.len());
    log.push(Change {
        rule: "post-keep",
        files: kept.into_iter().collect(),
        outcome: "deferred/composite passes that draw part of the screen or blend keep the buffer's other texels \
                  (Horizon clears each pass's target)"
            .into(),
    });
}
