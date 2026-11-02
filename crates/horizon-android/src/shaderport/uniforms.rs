//! zero-uniforms: Horizon passes uniforms it does not know yet as zero (its log: "zero for now").
//! Those that follow from ones it does pass are rebuilt from them; those whose zero breaks
//! drawing get Iris' usual value; a declared default (`uniform float cloudHeight = 192.0;`,
//! which GL keeps when nothing sets it and Horizon ignores) becomes a constant.
//! Runs on every pack; kept per file unless a program stops loading.

use std::collections::BTreeMap;

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Change, Edits, sources};
use crate::shaderport::load::{Eval, evaluate};

pub const NAME: &str = "zero-uniforms";
pub const WHY: &str = "Horizon passes uniforms it does not support yet as zero; those that follow from supported ones are computed from them (`cameraPositionFract` = `fract(cameraPosition)`), `alphaTestRef` gets Iris' 0.1 (at zero, see-through texels draw black: Mellow's hand), and declared defaults (`= 192.0`) are kept";

/// A zero uniform: the type it must be declared with, what replaces it, and the supported
/// uniforms that needs (type, name).
struct Fill {
    name: &'static str,
    ty: &'static str,
    expr: &'static str,
    deps: &'static [(&'static str, &'static str)],
}

const CAM: &[(&str, &str)] = &[("vec3", "cameraPosition")];
const PREV: &[(&str, &str)] = &[("vec3", "previousCameraPosition")];

const FILLS: &[Fill] = &[
    Fill { name: "cameraPositionFract", ty: "vec3", expr: "fract(cameraPosition)", deps: CAM },
    Fill { name: "cameraPositionInt", ty: "ivec3", expr: "ivec3(floor(cameraPosition))", deps: CAM },
    Fill { name: "previousCameraPositionFract", ty: "vec3", expr: "fract(previousCameraPosition)", deps: PREV },
    Fill { name: "previousCameraPositionInt", ty: "ivec3", expr: "ivec3(floor(previousCameraPosition))", deps: PREV },
    // first person: the eye is the camera (relativeEyePosition, eye minus camera, stays zero)
    Fill { name: "eyePosition", ty: "vec3", expr: "cameraPosition", deps: CAM },
    Fill {
        name: "playerLookVector",
        ty: "vec3",
        expr: "(mat3(gbufferModelViewInverse) * vec3(0.0, 0.0, -1.0))",
        deps: &[("mat4", "gbufferModelViewInverse")],
    },
    Fill {
        name: "gbufferPreviousModelViewInverse",
        ty: "mat4",
        expr: "inverse(gbufferPreviousModelView)",
        deps: &[("mat4", "gbufferPreviousModelView")],
    },
    Fill { name: "farPlane", ty: "float", expr: "far", deps: &[("float", "far")] },
    Fill { name: "alphaTestRef", ty: "float", expr: "0.1", deps: &[] },
    Fill { name: "isRightHanded", ty: "bool", expr: "true", deps: &[] },
    Fill { name: "isRightHanded", ty: "int", expr: "1", deps: &[] },
    Fill { name: "cloudHeight", ty: "float", expr: "192.0", deps: &[] },
];

/// Names seen as "zero for now" in game (nightly 7aa0e28): a declared default for one of
/// these becomes a constant.
const ZERO: &[&str] = &[
    "relativeEyePosition", "lightningBoltPosition", "cameraPositionFract", "cloudHeight", "eyePosition",
    "gbufferPreviousModelViewInverse", "centerDepthSmooth", "heavyFog", "currentRenderedItemId",
    "dhRenderDistance", "velocity", "previousCameraPositionInt", "previousCameraPositionFract", "playerMood",
    "playerLookVector", "isElytraFlying", "cameraPositionInt", "dhProjectionInverse", "dhProjection",
    "snowStorm", "sandStorm", "vxRenderDistance", "ditherShift", "vxProjInv", "vxModelViewInv", "taaOffset",
    "fixedTaaCorrection", "endFlashIntensity", "dhPreviousProjection", "dhFarPlane", "vxProj", "vxModelView",
    "alphaTestRef", "dhNearPlane", "bedrockLevel", "endFlashPosition", "farPlane", "isRightHanded", "isDead",
    "exitWater",
];

/// `uniform TYPE NAME;` or `uniform TYPE NAME = INIT;` on one line: (type, name, init).
fn uniform_decl(line: &str) -> Option<(&str, &str, Option<&str>)> {
    let code = line.split("//").next()?.trim();
    let rest = code.strip_prefix("uniform")?.strip_suffix(';')?;
    if !rest.starts_with(char::is_whitespace) || rest.contains(',') || rest.contains('[') {
        return None;
    }
    let (decl, init) = match rest.split_once('=') {
        Some((d, i)) => (d, Some(i.trim())),
        None => (rest, None),
    };
    let words: Vec<&str> = decl.split_whitespace().collect();
    let [ty, name] = words[..] else { return None };
    Some((ty, name, init))
}

fn guarded(ty: &str, name: &str) -> String {
    format!("#ifndef HZP_U_{name}\n#define HZP_U_{name}\nuniform {ty} {name};\n#endif\n")
}

/// The edits for every file: zero uniforms replaced, and the supported uniforms they need
/// declared once (each declaration guarded so a program never gets two).
fn edits(files: &Files) -> Edits {
    let srcs = sources(files);
    let mut out = Edits::new();
    // which deps the replacements need anywhere: their declarations get the guard
    let mut needed: BTreeMap<&str, &str> = BTreeMap::new();
    let mut plans: Vec<(String, String, Vec<String>)> = Vec::new();
    for (path, text) in &srcs {
        let mut new = String::with_capacity(text.len());
        let mut what = Vec::new();
        for line in text.split_inclusive('\n') {
            let nl = if line.ends_with('\n') { "\n" } else { "" };
            let Some((ty, name, init)) = uniform_decl(line) else {
                new.push_str(line);
                continue;
            };
            let indent = &line[..line.len() - line.trim_start().len()];
            if let Some(f) = FILLS.iter().find(|f| f.name == name && f.ty == ty) {
                let expr = match (init, f.deps.is_empty()) {
                    (Some(i), true) => i,
                    _ => f.expr,
                };
                if f.deps.is_empty() {
                    new.push_str(&format!("{indent}const {ty} {name} = {expr};{nl}"));
                } else {
                    for (dty, dname) in f.deps {
                        needed.insert(dname, dty);
                        new.push_str(&guarded(dty, dname));
                    }
                    new.push_str(&format!("#ifndef {name}\n#define {name} {expr}\n#endif{nl}"));
                }
                what.push(name.to_owned());
            } else if let Some(i) = init.filter(|_| ZERO.contains(&name)) {
                new.push_str(&format!("{indent}const {ty} {name} = {i};{nl}"));
                what.push(format!("{name} (its default)"));
            } else {
                new.push_str(line);
            }
        }
        plans.push((path.clone(), new, what));
    }
    // guard the pack's own declarations of the deps
    for (path, text, what) in plans {
        let mut new = String::with_capacity(text.len());
        let mut guards = 0;
        let mut in_ours = 0;
        for line in text.split_inclusive('\n') {
            // our own guarded block: `#ifndef HZP_U_x` … `#endif`
            if line.starts_with("#ifndef HZP_U_") {
                in_ours = 3;
                new.push_str(line);
                continue;
            }
            if in_ours > 0 {
                in_ours -= 1;
                new.push_str(line);
                continue;
            }
            if let Some((ty, name, None)) = uniform_decl(line)
                && needed.get(name) == Some(&ty)
            {
                let nl = if line.ends_with('\n') { "" } else { "\n" };
                new.push_str(&format!("#ifndef HZP_U_{name}\n#define HZP_U_{name}\n{line}{nl}#endif\n"));
                guards += 1;
                continue;
            }
            new.push_str(line);
        }
        if what.is_empty() && guards == 0 {
            continue;
        }
        let mut detail = Vec::new();
        if !what.is_empty() {
            detail.push(what.join(", "));
        }
        if guards > 0 {
            detail.push(format!("{guards} declaration(s) guarded"));
        }
        out.insert(path, (new, detail.join("; ")));
    }
    out
}

fn regressed(before: &Eval, after: &Eval) -> bool {
    (after.panicked() && !before.panicked())
        || before
            .dims
            .iter()
            .zip(&after.dims)
            .any(|(b, a)| a.skipped.iter().any(|s| b.loaded.contains(&s.program)))
}

/// Applies `edits` if nothing stops loading, else tries halves. Returns the files kept.
fn keep(files: &mut Files, best: &mut Eval, edits: Edits, depth: u32) -> Vec<String> {
    let saved: Vec<(String, Option<String>)> = edits.keys().map(|k| (k.clone(), files.text(k))).collect();
    for (k, (t, _)) in &edits {
        files.set_text(k, t.clone());
    }
    match evaluate(files) {
        Ok(e) if !regressed(best, &e) && e.score() <= best.score() => {
            *best = e;
            return edits.keys().cloned().collect();
        }
        _ => {}
    }
    for (k, t) in saved {
        if let Some(t) = t {
            files.set_text(&k, t);
        }
    }
    if edits.len() < 2 || depth >= 5 {
        return Vec::new();
    }
    let mut first = edits;
    let mid = first.keys().nth(first.len() / 2).unwrap().clone();
    let second = first.split_off(&mid);
    let mut kept = keep(files, best, first, depth + 1);
    kept.extend(keep(files, best, second, depth + 1));
    kept
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let edits = edits(files);
    if edits.is_empty() {
        return;
    }
    let Ok(mut best) = evaluate(files) else { return };
    let mut names: Vec<String> = edits
        .values()
        .flat_map(|(_, d)| d.split("; ").next().unwrap_or("").split(", ").map(str::to_owned).collect::<Vec<_>>())
        .filter(|n| !n.is_empty() && !n.contains("guarded"))
        .collect();
    names.sort();
    names.dedup();
    let total = edits.len();
    let kept = keep(files, &mut best, edits, 0);
    println!("  {NAME}: {} of {total} file(s) kept ({})", kept.len(), names.join(", "));
    log.push(Change {
        rule: NAME,
        files: kept.clone(),
        outcome: if kept.len() == total {
            format!("filled: {}", names.join(", "))
        } else if kept.is_empty() {
            "not kept (a program stopped loading)".into()
        } else {
            format!("filled: {} ({} of {total} files; the rest made a program stop loading)", names.join(", "), kept.len())
        },
    });
}

/// Pass `matrix-order`: in `uniform.*` / `variable.*` expressions, Iris reads `m.a.b` as column a,
/// row b (JOML's `m01` is column 0, row 1), and packs are written for that; Horizon reads it as
/// row a, column b (`custom.rs`), so a pack's own matrix products come out transposed (Noble's
/// world-space sun vector pointed the wrong way). The two numbers are swapped.
pub fn matrix_order(files: &mut Files, log: &mut Vec<Change>) {
    let Some(props) = files.text("shaders.properties") else { return };
    let mut n = 0;
    let mut out = String::with_capacity(props.len());
    for line in props.split_inclusive('\n') {
        let t = line.trim_start();
        if !(t.starts_with("uniform.") || t.starts_with("variable.")) {
            out.push_str(line);
            continue;
        }
        let Some(eq) = line.find('=') else {
            out.push_str(line);
            continue;
        };
        out.push_str(&line[..=eq]);
        let b = line[eq + 1..].as_bytes();
        let mut i = 0;
        while i < b.len() {
            // `<ident>.<d>.<d>` not followed by another identifier character or digit.
            if i > 0
                && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')
                && i + 4 <= b.len()
                && b[i] == b'.'
                && b[i + 1].is_ascii_digit()
                && b[i + 2] == b'.'
                && b[i + 3].is_ascii_digit()
                && !b.get(i + 4).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'.')
            {
                // Only after an identifier that starts with a letter (not `1.0.5`).
                let start = (0..i).rev().take_while(|&j| b[j].is_ascii_alphanumeric() || b[j] == b'_').last().unwrap_or(i);
                if b[start].is_ascii_alphabetic() || b[start] == b'_' {
                    out.push('.');
                    out.push(b[i + 3] as char);
                    out.push('.');
                    out.push(b[i + 1] as char);
                    i += 4;
                    n += 1;
                    continue;
                }
            }
            out.push(b[i] as char);
            i += 1;
        }
    }
    if n == 0 {
        return;
    }
    files.set_text("shaders.properties", out);
    println!("  matrix-order: {n} matrix element(s) swapped in shaders.properties");
    log.push(Change {
        rule: "matrix-order",
        files: vec!["shaders.properties".into()],
        outcome: format!("{n} `m.a.b` element(s) swapped: Iris reads column a, row b; Horizon row a, column b"),
    });
}
