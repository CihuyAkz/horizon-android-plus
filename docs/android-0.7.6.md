# Horizon Android 0.7.6

This update targets touch controls, initial menu navigation and diagnostic recording.

- Optional **Swap hands button**, off by default. It uses the normal swap-hands action and supports the layout editor.
- **Raise shield with: Sneak / Button**. The separate button appears when a blocking item is available. Shield input selects the blocking hand without consuming the main-hand item or interacting with a targeted block.
- Tap-control schemes show a crosshair for bows, crossbows, tridents and throwable items. Their use gestures aim through the center; other items retain touch targeting.
- **Horizontal sensitivity** and **Vertical sensitivity** calibrate camera input independently. Equal physical drag distances now produce equal default turns on both axes, including portrait screens.
- Flint and steel, fire charges, bone meal, buckets and other surface actions use short taps. Food, charged weapons and throwable projectiles retain hold-to-use behavior. Ordinary building blocks keep tap-to-place / hold-to-mine.
- Touch, gyro and controller camera movement count toward the look-around tutorial. Completed tutorial stages remain saved across worlds; an unfinished stage may appear again.
- Hardware D-pad navigation works in the initial title menu before the full game loads. Focus acquisition retries after widgets become available.
- GPU profiling and performance recording share one report session. Both launcher controls use the same button style. Reports include the requested profiling mode and whether encoder timestamps are supported; unsupported hardware cannot provide these stage timings.
- Installation cleanup waits for active resource preparation to finish; the final asset index is saved from verified memory rather than reopened from Android’s evictable cache. This prevents a concurrent startup cleanup from deleting files needed by a reinstall (#30). The original device report does not include a log, so a clean installation on that affected device still needs verification.
- Stable upstream history merged through `7aa0e28fa` (673 incoming commits, including merge history): rendering, chunk work, resource packs, gameplay, network and mod API changes. Android input, presentation bounds, upload budgets and sandbox `.hmod` support are retained.

Native `.hnmod` compilation on Android remains unsupported. Java Fabric, Forge and NeoForge mods are not supported.

The #34 Mali-G52 startup report identified a 1024-layer hardware limit with 1291
sprites. Renderer construction now selects a supported per-array size and
regroups the already prepared mip bytes across existing arrays. Shader packs use
the same actual split. Texture resolution, mipmaps and colors are retained;
verification on the originally affected phone is still required.

## Crash diagnostics

- Always-on bounded local diagnostic log, independent of performance recording.
- Current/previous process markers with build/ABI, last startup/game phase,
  a five-second memory heartbeat and handled game errors. An unclean marker is
  evidence of interruption, not proof of a crash: Android may kill a process.
- Android 11+ exit collection runs on a worker. Reports distinguish native/Java
  crashes, ANR and low-memory kills, retain OS memory samples and explicitly report
  unavailable/failed collection. Trace availability depends on Android.
- Settings > Diagnostic report exports a local ZIP and opens Reports. It includes
  bounded log tails, exit history, available panic reports and readable summaries
  of the crashed native thread (module/PC/Build ID). Raw tombstone memory/register
  dumps, accounts and worlds are not included. Redaction is best effort: review
  before sharing. Nothing is uploaded automatically.
- Release packaging keeps matching private native symbols under
  `.local/android/native-symbols/`, outside APKs and release attachments. Final
  Android libraries carry an ELF Build ID for matching native crash frames.

See [diagnostic collection and limitations](android-diagnostics.md).

## Validation

Local checks passed: 43 touch tests (including layout migration and per-axis calibration), 8 installer tests, 9 diagnostics/timing tests, 26 content tests (one fixture-dependent test skipped), 13 Android tooling tests, ARM64 Clippy and ARMv7 cargo check. Public-tree and whitespace checks passed.

The change is a release candidate until the device checklist below passes. Local source tests do not establish performance or compatibility on every phone.

See [the test checklist](android-0.7.6-testing.md).
