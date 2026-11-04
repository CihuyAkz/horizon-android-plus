Horizon Android Plus - GitHub Actions compile fix

Target branch: android
Failed workflow run: 41472050027
Build commit: 758c1fb545d5573b792da22578ed6d1d079a32d1

Replace the two source files in the repository with the files at the same
relative paths in this archive.

Changes:
- crates/horizon-client/src/android/documents.rs
  Uses the existing native_ime::pick_document() entry point and removes the
  call to the missing platform::open_content_document_picker() symbol.
- crates/horizon-client/src/content_files.rs
  Restores extract_shader_archive() and extract_shader_archive_with_progress(),
  including cancellation, extraction progress, wrapper-folder handling,
  path traversal/symlink checks, and expanded-size limits.

No package ID, application name, assets, signing configuration, or workflow
configuration is changed.

Validation performed: source symbol checks, patch whitespace checks, ZIP
integrity. The Rust/Android build was not run in this environment because
cargo/rustc are not installed; rerun the failed GitHub Actions workflow to
confirm the native build.
