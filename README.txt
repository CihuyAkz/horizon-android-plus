Horizon Android Plus — shader in-game popup adjustment

This ZIP contains the four updated source files from the previous shader overlay change,
with the shader toast notification restored.

Behavior:
- The full-screen shader import/conversion modal is suppressed while a shader operation
  is running during gameplay.
- Gameplay input is not captured by that modal while the game is active.
- Small Android in-game system-message toasts are retained, including shader import /
  conversion status messages.
- Shader conversion, progress state, cancellation, and conflict handling remain intact.
- The conflict-resolution UI remains available in the launcher after returning from game.
- Orientation settings are not changed.

Copy the files while preserving their directory paths.
Native APK compilation has not been performed in this environment.
