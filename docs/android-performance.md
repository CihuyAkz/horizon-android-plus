# Android performance reports

In the launcher, open Settings, enable **Performance recording**, then press
Play. Reproduce the slow startup, travel or other problem during normal play.
Return to the launcher and select **Performance report → Open in Files / Reports**.
Copy or share the `.jsonl` file from **Horizon Android → Reports** in system Files.

Recording is off by default. Each run is limited to 30 minutes and 8 MiB; the
three latest internal reports are retained. Opening Reports does not create copies.
Copies exported by the user remain in their chosen destination. Older versions'
copies in Download/HorizonAndroid are left untouched.
Reports are written on a bounded background queue and flushed once per sample;
if Android kills the process, completed samples remain available on next launch.
No general storage permission is needed to browse the document provider.

The report contains:

- One-second frame/CPU/GPU summaries with percentiles, maxima and slow events.
- Named render/server CPU spans: elapsed and thread CPU time, counts and maxima.
- Worker tasks: saved-chunk loading, generation, decoration, lighting, meshing
  and saving; concurrent task totals are reported separately from frame timings.
- Streaming queues, dirty light, pending mesh uploads and cumulative upload bytes.
- Terrain draw calls, resident sections, GPU allocation counters where available,
  resolution, render/simulation distances, presentation mode and effective FPS cap.
- Focus/pause/idle state, underwater state and entity/particle counts.
- Camera yaw/pitch and player velocity for distinguishing a changed view or movement
  from a stationary scene. Player coordinates remain excluded.
- Buffer creation, error-scope resolution and arena growth-copy CPU timings.
- The source commit and tracked-change flag when built with `android_build.py`,
  so reports from different builds with the same version can be distinguished.
- Android power-saving mode and reported thermal status (Android 10+), without
  root or access to device identifiers. A thermal status of 0 means none, 1 light,
  2 moderate, 3 severe, 4 critical, 5 emergency, 6 shutdown; unavailable is null.

These identify expensive stages and correlate spikes with background work. Nested
spans overlap and must not be added together. Wall time exceeding thread CPU time
can mean scheduling or waiting; it does not prove a particular lock is responsible.
GPU timing is asynchronous and can be missing; this is not a per-shader GPU profiler.
Recording itself adds timing overhead, so compare benchmark runs with recording off.

There is no automatic upload. Reports do not include account tokens, nicknames,
chat, server addresses, world names, seeds or player coordinates.

Developer regression check: `python tools/test_android_performance.py`. It runs
the actual recorder/statistics/timing code in a host harness and substitutes only
Android publication. The isolated storage probe tests provider access separately.
