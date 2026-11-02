//! `face-normals`: a fix for how Horizon draws, not for what it loads.
//!
//! Horizon (seen on nightly 7aa0e28, source a46ce68 `pack_atlas.rs`) gives every terrain vertex
//! the normal `(0, 1, 0)` and the tangent `(1, 0, 0)` unless the GPU runs its "flat" section
//! draws, which compute the real ones. Where they are off, a pack lights every block side as if
//! it were a top.
//!
//! The real face normal is still there per pixel: the cross product of the screen derivatives
//! of the view position. This rule, in `gbuffers_terrain` and `gbuffers_water`:
//! 1. finds the varyings the vertex shader derives from `gl_Normal` / `at_tangent` only through
//!    matrices, `normalize`, `cross` and constructors, and in which space they end up
//!    (model, view or world),
//! 2. passes the view position to the fragment shader as one more varying,
//! 3. in the fragment shader, before the pack's `main`, builds the rotation that takes the fake
//!    frame (tangent x, normal y) to the real face's frame and applies it to those varyings.
//!
//! Since Horizon's normal is the same constant everywhere, rotating what the pack derived from
//! it gives what the pack would have derived from the real one. Lighting the pack does in the
//! vertex shader itself (some light-weight packs) cannot be fixed this way.
//!
//! Mechanics: each stage's `main` is renamed (`#define main hz_pack_main`) and a new `main`
//! wraps it. Each fixed varying's declaration becomes, behind `#ifdef HZ_NFIX_<name>` (defined
//! only by the two wrappers of a fixed program), a varying `hz_nfix_in_<name>` plus a plain
//! global of the old name: the pack's code is left as it is. Kept only if no program that
//! loaded stops loading.

use std::collections::{BTreeMap, BTreeSet};

use horizon_shaders::{Macros, preprocess};

use crate::shaderport::files::Files;
use crate::shaderport::fix::Change;
use crate::shaderport::glsl::{after_version, is_ident_byte};
use crate::shaderport::load::{Eval, evaluate};

pub const NAME: &str = "face-normals";
pub const WHY: &str = "Horizon gives every terrain face the normal (0,1,0) and tangent (1,0,0) when its flat section draws are off, so block sides are lit like tops; the fragment shader now rebuilds the real face frame from screen derivatives and rotates the varyings the pack derived from the normal and tangent";

/// The programs that draw Horizon's terrain vertices.
const PROGRAMS: &[&str] = &["gbuffers_terrain", "gbuffers_water"];

/// Functions a normal may pass through and still be a rotated copy of it.
const LINEAR: &[&str] = &[
    "normalize", "fastNormalize", "vec3", "vec4", "mat3", "cross", "transpose", "inverse", "float",
    "sign",
];
/// Values that are not directions: an expression mixing these in is not a rotated normal
/// (`viewVector = tbn * (gl_ModelViewMatrix * gl_Vertex).xyz`).
const NOT_DIRECTIONS: &[&str] = &[
    "gl_Vertex", "vaPosition", "gl_Position", "gl_Color", "vaColor", "vaUV0", "mc_midTexCoord",
    "at_midBlock", "cameraPosition", "gl_MultiTexCoord0", "gl_MultiTexCoord1", "gl_MultiTexCoord2",
];
const WORLD_MATRICES: &[&str] = &[
    "gbufferModelViewInverse",
    "shadowModelViewInverse",
    "gl_ModelViewMatrixInverse",
    "modelViewMatrixInverse",
];
const VIEW_MATRICES: &[&str] = &[
    "gl_NormalMatrix",
    "gl_ModelViewMatrix",
    "gbufferModelView",
    "normalMatrix",
    "modelViewMatrix",
];
const TYPES: &[&str] = &["vec3", "vec4", "mat3"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Space {
    /// Horizon's model space: world-oriented, camera-relative.
    Model,
    View,
    World,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Taint {
    From(Space),
    /// Also assigned something else, or in a way that is not a rotation: left alone.
    Bad,
}

#[derive(Clone, Debug)]
struct Fixed {
    name: String,
    ty: String,
    space: Space,
}

#[derive(Clone, Debug)]
struct Program {
    /// `world0/gbuffers_terrain` (no extension).
    stem: String,
    fixed: Vec<Fixed>,
}

/// Applies the rule program by program, keeping each one whose pack still loads as before.
pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let Ok(mut best) = evaluate(files) else { return };
    let mut kept_files: BTreeSet<String> = BTreeSet::new();
    let mut kept_names: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for program in analyze(files) {
        if program.fixed.is_empty() {
            continue;
        }
        let edits = edits_for(files, &program);
        if edits.is_empty() {
            continue;
        }
        let saved: Vec<(String, Option<Vec<u8>>)> =
            edits.keys().map(|k| (k.clone(), files.remove(k))).collect();
        for (k, data) in &saved {
            files.restore(k, data.clone());
        }
        for (k, t) in &edits {
            files.set_text(k, t.clone());
        }
        let ok = match evaluate(files) {
            Ok(e) if !regressed(&best, &e) && e.score() <= best.score() => Some(e),
            _ => None,
        };
        match ok {
            Some(e) => {
                best = e;
                kept_files.extend(edits.keys().cloned());
                kept_names.push(format!(
                    "{} ({})",
                    program.stem,
                    program
                        .fixed
                        .iter()
                        .map(|f| format!("{} {:?}", f.name, f.space).to_lowercase())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                println!("  {NAME}: {} fixed: {}", program.stem, kept_names.last().unwrap());
            }
            None => {
                for (k, data) in saved {
                    files.restore(&k, data);
                }
                // A stage file that only includes another program's file fixed already
                // (Mellow's `world0/gbuffers_water` includes `program/gbuffers_water`).
                let covered = files.pack().is_ok_and(|pack| {
                    preprocess(&pack, &format!("{}.fsh", program.stem), &Macros::standard())
                        .code
                        .contains("hz_nfix_")
                });
                if covered {
                    kept_names.push(format!("{} (through its include)", program.stem));
                    println!("  {NAME}: {} fixed through its include", program.stem);
                } else {
                    failed.push(program.stem.clone());
                    println!("  {NAME}: {} not kept (a program stopped loading)", program.stem);
                }
            }
        }
    }
    if kept_files.is_empty() && failed.is_empty() {
        return;
    }
    let mut outcome = if kept_names.is_empty() {
        "not kept".to_owned()
    } else {
        format!("varyings rotated in {}", kept_names.join("; "))
    };
    if !failed.is_empty() {
        outcome += &format!("; not kept in {} (a program stopped loading)", failed.join(", "));
    }
    log.push(Change {
        rule: NAME,
        files: kept_files.into_iter().collect(),
        outcome,
    });
}

fn regressed(before: &Eval, after: &Eval) -> bool {
    if after.panicked() && !before.panicked() {
        return true;
    }
    before
        .dims
        .iter()
        .zip(&after.dims)
        .any(|(b, a)| a.skipped.iter().any(|s| b.loaded.contains(&s.program)))
}

/// Every terrain program with both stages, and the varyings to fix in it.
fn analyze(files: &Files) -> Vec<Program> {
    let Ok(pack) = files.pack() else { return Vec::new() };
    let macros = Macros::standard();
    let mut out = Vec::new();
    for key in files.entries.keys() {
        let Some(stem) = key.strip_prefix("shaders/").and_then(|p| p.strip_suffix(".vsh")) else {
            continue;
        };
        let base = stem.rsplit('/').next().unwrap_or(stem);
        if !PROGRAMS.contains(&base) || files.text(&format!("{stem}.fsh")).is_none() {
            continue;
        }
        let vsh = preprocess(&pack, &format!("{stem}.vsh"), &macros).code;
        let fsh = preprocess(&pack, &format!("{stem}.fsh"), &macros).code;
        let outs = varyings(&vsh, &["varying", "out"]);
        let ins = varyings(&fsh, &["varying", "in"]);
        let taints = taint(&vsh);
        if std::env::var_os("HZ_TRACE").is_some() {
            for (name, ty) in &outs {
                eprintln!(
                    "  [trace] {stem}: out {ty} {name}: {:?}, fragment {:?}",
                    taints.get(name),
                    ins.get(name)
                );
            }
        }
        let mut fixed = Vec::new();
        for (name, ty) in &outs {
            if let (Some(Taint::From(space)), Some(fty)) = (taints.get(name), ins.get(name))
                && fty == ty
            {
                fixed.push(Fixed { name: name.clone(), ty: ty.clone(), space: *space });
            }
        }
        out.push(Program { stem: stem.to_owned(), fixed });
    }
    out
}

/// Statement text with what precedes it in the same block taken off (`}`, `{`, directives).
fn statements(code: &str) -> Vec<String> {
    code.split(';')
        .map(|s| {
            let s = s.rsplit(['{', '}']).next().unwrap_or(s);
            s.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_owned()
        })
        .collect()
}

/// A declaration's parts: (text before the names, storage, type, names), for
/// `[layout(...)] [qualifiers] storage [precision] type a, b`.
fn declaration(stmt: &str) -> Option<(String, String, String, Vec<String>)> {
    let mut rest = stmt.trim();
    let mut prefix = String::new();
    if rest.starts_with("layout") {
        let close = rest.find(')')?;
        prefix.push_str(&rest[..=close]);
        prefix.push(' ');
        rest = rest[close + 1..].trim_start();
    }
    let mut storage = None;
    let mut ty = None;
    let mut words = rest.split_whitespace().peekable();
    while let Some(w) = words.next() {
        match w {
            "flat" | "smooth" | "noperspective" | "centroid" | "invariant" | "highp" | "mediump"
            | "lowp" => {}
            "varying" | "in" | "out" => storage = Some(w.to_owned()),
            t if TYPES.contains(&t) && storage.is_some() => {
                ty = Some(t.to_owned());
                prefix.push_str(w);
                break;
            }
            _ => return None,
        }
        prefix.push_str(w);
        prefix.push(' ');
    }
    let ty = ty?;
    let names: String = words.collect::<Vec<_>>().join(" ");
    let names: Vec<String> = names.split(',').map(|n| n.trim().to_owned()).collect();
    if names.iter().any(|n| n.is_empty() || !n.bytes().all(is_ident_byte)) {
        return None;
    }
    Some((prefix, storage?, ty, names))
}

/// Varyings of a stage by name, with their type (vec3, vec4, mat3 only).
fn varyings(code: &str, storages: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for s in statements(code) {
        if let Some((_, storage, ty, names)) = declaration(&s)
            && storages.contains(&storage.as_str())
        {
            for n in names {
                out.insert(n, ty.clone());
            }
        }
    }
    out
}

fn identifiers(text: &str) -> Vec<(String, bool)> {
    // (identifier, followed by `(`)
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if is_ident_byte(b[i]) && !b[i].is_ascii_digit() && (i == 0 || b[i - 1] != b'.') {
            let start = i;
            while i < b.len() && is_ident_byte(b[i]) {
                i += 1;
            }
            let called = text[i..].trim_start().starts_with('(');
            out.push((text[start..i].to_owned(), called));
        } else if is_ident_byte(b[i]) {
            while i < b.len() && is_ident_byte(b[i]) {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The `=` of a plain assignment in `stmt` (not `==`, `<=`, `+=`...).
fn assign_at(stmt: &str) -> Option<usize> {
    let b = stmt.as_bytes();
    for i in 0..b.len() {
        if b[i] != b'=' {
            continue;
        }
        let prev = if i > 0 { b[i - 1] } else { b' ' };
        let next = b.get(i + 1).copied().unwrap_or(b' ');
        if next == b'=' || b"=<>!+-*/%&|^".contains(&prev) {
            return None;
        }
        return Some(i);
    }
    None
}

/// `rhs` with every `name.w` (or `.a`, `.q`) written as `1.0`.
fn scalar_w(rhs: &str) -> String {
    let b = rhs.as_bytes();
    let mut out = String::with_capacity(rhs.len());
    let mut i = 0;
    while i < b.len() {
        if is_ident_byte(b[i]) && (i == 0 || (!is_ident_byte(b[i - 1]) && b[i - 1] != b'.')) {
            let start = i;
            while i < b.len() && is_ident_byte(b[i]) {
                i += 1;
            }
            let is_w = b.get(i) == Some(&b'.')
                && b.get(i + 1).is_some_and(|c| b"waq".contains(c))
                && !b.get(i + 2).is_some_and(|c| is_ident_byte(*c));
            if is_w && !b[start].is_ascii_digit() {
                out.push_str("1.0");
                i += 2;
            } else {
                out.push_str(&rhs[start..i]);
            }
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// A binary `+` or `-` in `rhs` (a rotation has none; a unary minus is fine).
fn adds(rhs: &str) -> bool {
    let b = rhs.as_bytes();
    (0..b.len()).any(|i| {
        if b[i] != b'+' && b[i] != b'-' {
            return false;
        }
        // Binary when what comes before ends an operand (and it's not an exponent `1e-3`).
        let before = rhs[..i].trim_end();
        let Some(&p) = before.as_bytes().last() else { return false };
        let exponent = (p == b'e' || p == b'E')
            && before.len() >= 2
            && before.as_bytes()[before.len() - 2].is_ascii_digit();
        (is_ident_byte(p) || p == b')' || p == b']') && !exponent
    })
}

/// Every use of a tainted value in `rhs` takes the whole vector (`n`, `n.xyz`) or its `w`, not
/// single components (`mat3(t.x, b.x, n.x, ...)` builds a transposed frame).
fn whole_vectors(rhs: &str, t: &BTreeMap<String, Taint>) -> bool {
    let b = rhs.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !is_ident_byte(b[i]) || (i > 0 && (is_ident_byte(b[i - 1]) || b[i - 1] == b'.')) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && is_ident_byte(b[i]) {
            i += 1;
        }
        if !t.contains_key(&rhs[start..i]) || b.get(i) != Some(&b'.') {
            continue;
        }
        let sw_start = i + 1;
        let mut j = sw_start;
        while j < b.len() && is_ident_byte(b[j]) {
            j += 1;
        }
        let sw = &rhs[sw_start..j];
        if !["xyz", "rgb", "stp", "w", "a", "q"].contains(&sw) {
            return false;
        }
        i = j;
    }
    true
}

/// What each variable of the vertex shader holds: a rotated copy of the normal/tangent (and in
/// which space), or something else.
fn taint(vsh: &str) -> BTreeMap<String, Taint> {
    let mut seeds: BTreeMap<String, Taint> = BTreeMap::new();
    for seed in ["gl_Normal", "vaNormal", "at_tangent"] {
        seeds.insert(seed.into(), Taint::From(Space::Model));
    }
    // A function without parameters that returns a rotated normal (`mat3 get_tbn_matrix()`)
    // counts as one, called.
    for (name, body, ret) in zero_param_functions(vsh) {
        let inner = taint_with(&format!("{body};\nhz_nfix_ret = {ret};"), &seeds);
        if let Some(x) = inner.get("hz_nfix_ret") {
            seeds.insert(name, *x);
        }
    }
    taint_with(vsh, &seeds)
}

/// `TYPE name() { ... return EXPR; }` with a single `return`: (name, body before it, EXPR).
fn zero_param_functions(code: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(n) = code[from..].find("()") {
        let at = from + n;
        from = at + 2;
        let before = code[..at].trim_end();
        let name: String = before.chars().rev().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>().chars().rev().collect();
        let ty = before[..before.len() - name.len()].trim_end();
        if name.is_empty() || !TYPES.iter().any(|t| ty.ends_with(t)) {
            continue;
        }
        let rest = &code[at + 2..];
        if !rest.trim_start().starts_with('{') {
            continue;
        }
        let open = at + 2 + (rest.len() - rest.trim_start().len());
        let Some(close) = crate::shaderport::glsl::matching(code, open) else { continue };
        let body = &code[open + 1..close];
        let returns = crate::shaderport::glsl::find_word(body, "return");
        let [r] = returns.as_slice() else { continue };
        let Some(semi) = body[*r..].find(';') else { continue };
        out.push((name, body[..*r].to_owned(), body[*r + 6..*r + semi].trim().to_owned()));
    }
    out
}

fn taint_with(vsh: &str, seeds: &BTreeMap<String, Taint>) -> BTreeMap<String, Taint> {
    let mut t = seeds.clone();
    let mut plain: BTreeSet<String> = BTreeSet::new();
    let stmts = statements(vsh);
    for _ in 0..2 {
        for s in &stmts {
            let Some(eq) = assign_at(s) else { continue };
            let (lhs, rhs) = (s[..eq].trim(), &s[eq + 1..]);
            // `name`, `name.xyz`, or `type name`.
            let target = lhs.rsplit(|c: char| c.is_whitespace() || c == ')').next().unwrap_or("");
            // A column (`tbn[2] = ...`) counts as the whole matrix.
            let target = target.split('[').next().unwrap_or(target);
            let (name, swizzle) = match target.split_once('.') {
                Some((n, sw)) => (n, Some(sw)),
                None => (target, None),
            };
            if name.is_empty() || !name.bytes().all(is_ident_byte) || name.starts_with("gl_") {
                continue;
            }
            // `x.w` is a number (the tangent's handedness, a material), not a direction.
            let rhs_owned = scalar_w(rhs);
            let rhs = rhs_owned.as_str();
            let ids = identifiers(rhs);
            let used: Vec<Taint> = ids.iter().filter_map(|(i, _)| t.get(i).copied()).collect();
            if used.is_empty() {
                // Only the w of a vec4 (`n.w = material`) leaves the direction alone.
                if swizzle.is_some_and(|sw| sw.chars().all(|c| c == 'w' || c == 'a')) {
                    continue;
                }
                if t.contains_key(name) {
                    t.insert(name.into(), Taint::Bad);
                } else {
                    plain.insert(name.into());
                }
                continue;
            }
            // Only matrices, the normal's own copies and linear functions: anything else
            // (an added colour, a position, a light vector) makes it more than a rotation.
            let linear = ids.iter().all(|(i, called)| {
                if *called {
                    LINEAR.contains(&i.as_str()) || matches!(t.get(i), Some(Taint::From(_)))
                } else {
                    t.contains_key(i)
                        || WORLD_MATRICES.contains(&i.as_str())
                        || VIEW_MATRICES.contains(&i.as_str())
                        || i == "true"
                        || i == "false"
                }
            }) && !ids.iter().any(|(i, _)| NOT_DIRECTIONS.contains(&i.as_str()))
                && !adds(rhs)
                && whole_vectors(rhs, &t);
            let has = |set: &[&str]| ids.iter().any(|(i, _)| set.contains(&i.as_str()));
            let space = if used.contains(&Taint::Bad) || !linear || plain.contains(name) {
                Taint::Bad
            } else if has(WORLD_MATRICES) {
                Taint::From(Space::World)
            } else if has(VIEW_MATRICES) {
                Taint::From(Space::View)
            } else if used.windows(2).all(|w| w[0] == w[1]) {
                used[0]
            } else {
                Taint::Bad
            };
            let space = match (t.get(name), space) {
                (Some(old), new) if *old != new => Taint::Bad,
                _ => space,
            };
            t.insert(name.into(), space);
        }
    }
    t
}

/// The new texts (by `shaders/` path) that fix `program`, from the current files.
fn edits_for(files: &Files, program: &Program) -> BTreeMap<String, String> {
    let mut edits = BTreeMap::new();
    // Declarations, in whichever file holds them.
    let by_name: BTreeMap<&str, &Fixed> = program.fixed.iter().map(|f| (f.name.as_str(), f)).collect();
    for (key, data) in &files.entries {
        let Some(path) = key.strip_prefix("shaders/") else { continue };
        let ext = path.rsplit_once('.').map_or("", |p| p.1);
        if !["vsh", "fsh", "glsl", "inc", "h"].contains(&ext) {
            continue;
        }
        let text = String::from_utf8_lossy(data).into_owned();
        if let Some(new) = rewrite_declarations(&text, &by_name) {
            edits.insert(path.to_owned(), new);
        }
    }
    for stage in ["vsh", "fsh"] {
        let path = format!("{}.{stage}", program.stem);
        let text = edits.get(&path).cloned().or_else(|| files.text(&path)).unwrap_or_default();
        if text.contains("hz_pack_main") {
            return BTreeMap::new();
        }
        edits.insert(path, wrap(&text, program, stage == "vsh"));
    }
    edits
}

/// Each one-line declaration of a fixed varying becomes the guarded pair; `None` if none.
fn rewrite_declarations(text: &str, fixed: &BTreeMap<&str, &Fixed>) -> Option<String> {
    let mut out = String::with_capacity(text.len() + 256);
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        let body = line.split("//").next().unwrap_or(line);
        let trimmed = body.trim();
        let decl = trimmed
            .strip_suffix(';')
            .filter(|s| !s.contains(';') && !s.contains('{') && !s.contains('}'))
            .and_then(declaration);
        let Some((prefix, _, ty, names)) = decl else {
            out.push_str(line);
            continue;
        };
        let (hit, rest): (Vec<&String>, Vec<&String>) = names
            .iter()
            .partition(|n| fixed.get(n.as_str()).is_some_and(|f| f.ty == ty) && !text.contains(&format!("hz_nfix_in_{n};")));
        if hit.is_empty() {
            out.push_str(line);
            continue;
        }
        changed = true;
        let indent: String = line.chars().take_while(|c| c.is_whitespace() && *c != '\n').collect();
        if !rest.is_empty() {
            let rest: Vec<&str> = rest.iter().map(|s| s.as_str()).collect();
            out.push_str(&format!("{indent}{prefix} {};\n", rest.join(", ")));
        }
        for n in hit {
            out.push_str(&format!(
                "#ifdef HZ_NFIX_{n}\n{indent}{prefix} hz_nfix_in_{n}; {ty} {n};\n#else\n{indent}{prefix} {n};\n#endif\n"
            ));
        }
    }
    changed.then_some(out)
}

/// The program's stage file with `main` wrapped.
fn wrap(text: &str, program: &Program, vertex: bool) -> String {
    let version: u32 = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("#version"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(120);
    let storage = match (version < 130, vertex) {
        (true, _) => "varying",
        (false, true) => "out",
        (false, false) => "in",
    };
    let mut head = String::from("#define main hz_pack_main\n");
    for f in &program.fixed {
        head.push_str(&format!("#define HZ_NFIX_{}\n", f.name));
    }
    let at = after_version(text).unwrap_or(0);
    let mut out = format!("{}{}{}", &text[..at], head, &text[at..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n// {NAME} (horizon-shaderport)\n#undef main\n{storage} vec3 hz_nfix_vpos;\n"));
    if vertex {
        out.push_str("void main() {\n    hz_pack_main();\n");
        for f in &program.fixed {
            out.push_str(&format!("    hz_nfix_in_{0} = {0};\n", f.name));
        }
        out.push_str("    hz_nfix_vpos = (gl_ModelViewMatrix * gl_Vertex).xyz;\n}\n");
    } else {
        out.push_str(FRAGMENT_FRAME);
        out.push_str("void main() {\n    hz_nfix_frame();\n");
        for f in &program.fixed {
            let r = match f.space {
                Space::View => "hz_nfix_rv",
                Space::World | Space::Model => "hz_nfix_rw",
            };
            let n = &f.name;
            match f.ty.as_str() {
                "vec4" => out.push_str(&format!("    {n} = vec4({r} * hz_nfix_in_{n}.xyz, hz_nfix_in_{n}.w);\n")),
                _ => out.push_str(&format!("    {n} = {r} * hz_nfix_in_{n};\n")),
            }
        }
        out.push_str("    hz_pack_main();\n}\n");
    }
    out
}

/// The rotation from Horizon's frame (tangent x, normal y, bitangent z) to the face's, in
/// world (`hz_nfix_rw`) and view (`hz_nfix_rv`) space. Near-axis normals are snapped (block
/// faces), so distant faces don't shimmer.
const FRAGMENT_FRAME: &str = "mat3 hz_nfix_rw;
mat3 hz_nfix_rv;
void hz_nfix_frame() {
    vec3 hz_n = cross(dFdx(hz_nfix_vpos), dFdy(hz_nfix_vpos));
    hz_n = dot(hz_n, hz_n) > 1e-24 ? normalize(hz_n) : vec3(0.0, 0.0, 1.0);
    if (dot(hz_n, hz_nfix_vpos) > 0.0) hz_n = -hz_n;
    mat3 hz_v = mat3(gl_ModelViewMatrix);
    vec3 hz_w = transpose(hz_v) * hz_n;
    vec3 hz_a = abs(hz_w);
    float hz_m = max(hz_a.x, max(hz_a.y, hz_a.z));
    if (hz_m > 0.97) hz_w = step(vec3(hz_m), hz_a) * sign(hz_w);
    hz_w = normalize(hz_w);
    vec3 hz_t = abs(hz_w.y) > 0.9 ? vec3(1.0, 0.0, 0.0) : normalize(cross(vec3(0.0, 1.0, 0.0), hz_w));
    hz_nfix_rw = mat3(hz_t, hz_w, cross(hz_t, hz_w));
    hz_nfix_rv = hz_v * hz_nfix_rw * transpose(hz_v);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_rotated_copies_and_their_space() {
        let vsh = "varying vec3 normal; varying vec4 nm; varying vec3 binormal; varying vec3 tint; varying vec3 lit;\n\
                   void main() {\n normal = normalize(gl_NormalMatrix * gl_Normal);\n\
                   nm = vec4(mat3(gbufferModelViewInverse) * normal, 1.0);\n nm.w = 0.5;\n\
                   binormal = normalize(gl_NormalMatrix * cross(at_tangent.xyz, gl_Normal.xyz) * at_tangent.w);\n\
                   tint = gl_Color.rgb;\n lit = vec3(max(dot(normal, sunVec), 0.0));\n}\n";
        let t = taint(vsh);
        assert_eq!(t.get("normal"), Some(&Taint::From(Space::View)));
        assert_eq!(t.get("nm"), Some(&Taint::From(Space::World)));
        assert_eq!(t.get("binormal"), Some(&Taint::From(Space::View)));
        assert_eq!(t.get("tint"), None);
        assert_eq!(t.get("lit"), Some(&Taint::Bad));
        let vsh2 = "void main() {\n binormal = normalize(gl_NormalMatrix * cross(at_tangent.xyz, gl_Normal.xyz) * at_tangent.w);\n\
                    normal = normalize(gl_NormalMatrix * gl_Normal);\n tangent = normalize(gl_NormalMatrix * at_tangent.xyz);\n\
                    mat3 tbnMatrix = mat3(tangent.x, binormal.x, normal.x, tangent.y, binormal.y, normal.y, tangent.z, binormal.z, normal.z);\n\
                    viewVector = tbnMatrix * (gl_ModelViewMatrix * gl_Vertex).xyz;\n\
                    endTint = vec3(0.1, 0.2, 0.3) + 0.02 * (gl_NormalMatrix * gl_Normal).xyz;\n\
                    TBN = mat3(gbufferModelViewInverse) * (gl_NormalMatrix * mat3(tangent, cross(tangent, normal) * sign(at_tangent.w), normal));\n}\n";
        let t = taint(vsh2);
        assert_eq!(t.get("tangent"), Some(&Taint::From(Space::View)));
        assert_eq!(t.get("tbnMatrix"), Some(&Taint::Bad));
        assert_eq!(t.get("viewVector"), Some(&Taint::Bad));
        assert_eq!(t.get("endTint"), Some(&Taint::Bad));
        assert_eq!(t.get("TBN"), Some(&Taint::From(Space::World)));
        // Noble: column by column. Photon: through a function without parameters.
        let noble = "void main() {\n tbn[2] = mat3(gbufferModelViewInverse) * normalize(gl_NormalMatrix * gl_Normal);\n\
                     tbn[0] = mat3(gbufferModelViewInverse) * normalize(gl_NormalMatrix * at_tangent.xyz);\n\
                     tbn[1] = cross(tbn[0], tbn[2]) * sign(at_tangent.w);\n}\n";
        assert_eq!(taint(noble).get("tbn"), Some(&Taint::From(Space::World)));
        let photon = "mat3 get_tbn_matrix() {\n mat3 tbn;\n tbn[0] = mat3(gbufferModelViewInverse) * normalize(gl_NormalMatrix * at_tangent.xyz);\n\
                      tbn[2] = mat3(gbufferModelViewInverse) * normalize(gl_NormalMatrix * gl_Normal);\n\
                      tbn[1] = cross(tbn[0], tbn[2]) * sign(at_tangent.w);\n return tbn;\n}\n\
                      void main() {\n tbn = get_tbn_matrix();\n}\n";
        assert_eq!(taint(photon).get("tbn"), Some(&Taint::From(Space::World)));
        let v = varyings(vsh, &["varying"]);
        assert_eq!(v.get("nm").map(String::as_str), Some("vec4"));
    }

    #[test]
    fn rewrites_declarations_behind_a_guard() {
        let f = Fixed { name: "normal".into(), ty: "vec3".into(), space: Space::View };
        let map = BTreeMap::from([("normal", &f)]);
        let out = rewrite_declarations("flat in vec3 normal, other; // n\nin vec2 uv;\n", &map).unwrap();
        assert!(out.contains("flat in vec3 other;"));
        assert!(out.contains("#ifdef HZ_NFIX_normal\nflat in vec3 hz_nfix_in_normal; vec3 normal;\n#else\nflat in vec3 normal;\n#endif"));
        assert!(out.contains("in vec2 uv;"));
    }
}
