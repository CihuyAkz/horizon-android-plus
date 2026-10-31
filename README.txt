Horizon Android Plus - Android CI compile fixes

This source update addresses the Rust compile errors from GitHub Actions run 41208394347.

Changes:
- ui_theme.rs: exposes color::with_alpha as a public function so the Android launcher crate can call it.
- android/launcher_pages.rs: stores the Home action label in a String before borrowing it for button(), avoiding E0716.
- android/catalog.rs: marks two intentionally unused values with underscore names to remove warnings from that build.

The update is based on the native UI source ZIP previously prepared for this repository. It does not change the provider routing or launcher behavior.

Validation:
- Source-level checks confirm the referenced function is public and the temporary format! borrow is gone.
- ZIP integrity is checked after packaging.
- cargo check/build and Android APK compilation were not run in this environment because cargo and rustc are not installed.
