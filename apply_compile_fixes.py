#!/usr/bin/env python3
"""Apply the compile fixes for Android branch commit 267fdb4e8bbec4ab5ea31367f3a00d5c480c7656.
Run from the repository root: python apply_compile_fixes.py
Only the seven source files listed below are modified.
"""
from pathlib import Path
import sys

ROOT = Path.cwd()


def read(path: str) -> str:
    file = ROOT / path
    if not file.is_file():
        raise RuntimeError(f"Missing source file: {path}")
    return file.read_bytes().decode("utf-8")


def replace_exact(text: str, old: str, new: str, expected: int, label: str) -> str:
    count = text.count(old)
    if count != expected:
        raise RuntimeError(
            f"Unexpected source for {label}: expected {expected} occurrence(s), found {count}. "
            "No files have been modified."
        )
    return text.replace(old, new)


# Load and transform every file in memory first. A mismatch aborts before writes.
changes: dict[str, str] = {}

path = "launcher/src/skins/preview.rs"
text = read(path)
text = replace_exact(
    text,
    "horizon_core::player_texture::process_legacy_skin",
    "horizon_client::skins::process_legacy_skin",
    1,
    f"{path}: legacy skin processor path",
)
text = replace_exact(
    text,
    "horizon_core::player_texture::process_cape",
    "horizon_client::skins::process_cape",
    1,
    f"{path}: cape processor path",
)
changes[path] = text

path = "crates/horizon-client/src/android/chooser.rs"
text = read(path)
for before, after, label in (
    ("color::BG", "color::bg()", "background palette"),
    ("color::TEXT", "color::text()", "text palette"),
    ("color::MUTED", "color::muted()", "muted palette"),
    ("color::BORDER", "color::border()", "border palette"),
):
    count = text.count(before)
    if count < 1:
        raise RuntimeError(f"Missing {label} references in {path}. No files have been modified.")
    text = text.replace(before, after)
changes[path] = text

path = "crates/horizon-client/src/android/launcher.rs"
text = read(path)
text = replace_exact(
    text,
    "        crate::document_store::set_game_active(false);\n",
    "",
    2,
    f"{path}: unsupported document-store inactive flag",
)
text = replace_exact(
    text,
    "            crate::document_store::set_game_active(true);\n",
    "",
    1,
    f"{path}: unsupported document-store active flag",
)
changes[path] = text

path = "crates/horizon-client-render/src/android_runtime.rs"
text = read(path)
anchor = "use std::sync::atomic::{AtomicBool, Ordering};\n"
addition = '''use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// Android launcher overrides are read by the game renderer before its worker
// pools and sound cache are created. Zero selects the engine defaults.
static WORKER_LIMIT: AtomicU32 = AtomicU32::new(0);
static CACHE_BUDGET_MIB: AtomicU32 = AtomicU32::new(0);

pub fn configure_worker_limit(limit: u32) {
    WORKER_LIMIT.store(limit.min(64), Ordering::Relaxed);
}

pub fn worker_limit() -> u32 {
    WORKER_LIMIT.load(Ordering::Relaxed)
}

pub fn configure_cache_budget(mib: u32) {
    CACHE_BUDGET_MIB.store(mib.min(256), Ordering::Relaxed);
}

fn cache_budget_bytes() -> usize {
    let configured = CACHE_BUDGET_MIB.load(Ordering::Relaxed);
    let mib = if configured == 0 { 80 } else { configured };
    mib as usize * 1024 * 1024
}

/// Eighty percent of the selected reusable RAM budget is reserved for decoded sounds.
pub fn sound_cache_budget_bytes() -> usize {
    cache_budget_bytes() * 80 / 100
}

/// Twenty percent of the selected reusable RAM budget is reserved for empty mesh buffers.
pub fn mesh_cache_budget_bytes() -> usize {
    cache_budget_bytes() - sound_cache_budget_bytes()
}
'''
text = replace_exact(text, anchor, addition, 1, f"{path}: runtime configuration insertion point")
changes[path] = text

path = "crates/horizon-client-render/src/graphics.rs"
text = read(path)
old_method = '''    /// Worker threads for chunk generation and meshing.
    pub fn worker_threads(&self) -> usize {
        if self.workers > 0 {
            return self.workers as usize;
        }
        // Vanilla's count (Util.maxAllowedExecutorThreads: every core but
        // one). The pool never takes every core: a pool that fills them all
        // starves the render and server threads, and the frame rate collapses
        // while the workers saturate the CPU and its memory bandwidth. One
        // core left is enough with the workers behind those two threads
        // (horizon_threads: niced on Linux; on macOS and Windows the server
        // and render threads are raised above four cores, where vanilla
        // raises its server thread). Leaving two idled a core of a 4-thread
        // laptop while a world loaded: a teleport there took ~15% longer.
        let auto = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            // Preserve the Android fork's render/server headroom until phone profiling.
            .saturating_sub(if cfg!(target_os = "android") { 2 } else { 1 })
            .max(1);
        if self.battery { auto.div_ceil(2) } else { auto }
    }
'''
new_method = '''    /// Worker threads for chunk generation and meshing, including the Android launcher ceiling.
    pub fn worker_threads(&self) -> usize {
        let selected = if self.workers > 0 {
            self.workers as usize
        } else {
            // Vanilla's count (Util.maxAllowedExecutorThreads: every core but
            // one). The pool never takes every core: a pool that fills them all
            // starves the render and server threads, and the frame rate collapses
            // while the workers saturate the CPU and its memory bandwidth. One
            // core left is enough with the workers behind those two threads
            // (horizon_threads: niced on Linux; on macOS and Windows the server
            // and render threads are raised above four cores, where vanilla
            // raises its server thread). Leaving two idled a core of a 4-thread
            // laptop while a world loaded: a teleport there took ~15% longer.
            let auto = std::thread::available_parallelism()
                .map_or(2, |n| n.get())
                // Preserve the Android fork's render/server headroom until phone profiling.
                .saturating_sub(if cfg!(target_os = "android") { 2 } else { 1 })
                .max(1);
            if self.battery { auto.div_ceil(2) } else { auto }
        };
        let limit = crate::android_runtime::worker_limit() as usize;
        if limit == 0 { selected } else { selected.min(limit).max(1) }
    }
'''
text = replace_exact(text, old_method, new_method, 1, f"{path}: worker count implementation")
changes[path] = text

path = "crates/horizon-client-render/src/audio.rs"
text = read(path)
text = replace_exact(
    text,
    "        while self.bytes > CACHE_BYTES && self.entries.len() > 1 {",
    "        while self.bytes > crate::android_runtime::sound_cache_budget_bytes() && self.entries.len() > 1 {",
    1,
    f"{path}: decoded sound cache ceiling",
)
changes[path] = text

path = "crates/horizon-client-render/src/streaming.rs"
text = read(path)
text = replace_exact(
    text,
    "bytes > Self::BYTES - self.bytes",
    "bytes > crate::android_runtime::mesh_cache_budget_bytes().saturating_sub(self.bytes)",
    1,
    f"{path}: retained mesh cache ceiling",
)
changes[path] = text

# All source checks passed. Write only changed paths, preserving the original names.
for relative, content in changes.items():
    target = ROOT / relative
    temporary = target.with_name(target.name + ".compile-fix.tmp")
    temporary.write_bytes(content.encode("utf-8"))
    temporary.replace(target)

print("Applied Android compile fixes to exactly these files:")
for relative in changes:
    print(f" - {relative}")
