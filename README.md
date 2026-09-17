# Smowaudio

A lightweight replacement for SteelSeries Sonar / Elgato Wave Link / Voicemeeter on Windows 11.

- **Output channels:** Game, Chat, Media, Aux (the same as SteelSeries Sonar), each with volume, mute, and an optional
  parametric EQ, mixed to your headphones. Game is your Windows default output, so it also carries system sounds.
- **Virtual Mic:** Noise Removal (DeepFilterNet3 AI) → Noise Gate → Equalizer → Compressor → output gain.
- **Per-app routing:** assign any app to a channel from the Apps tab. Windows remembers it.
- **Starts fast:** the audio engine starts before the UI. Launched at sign-in, it sits in the tray and the WebView window is only created when you open it.

## One-time setup

1. **Install the virtual cables** from <https://vb-audio.com/Cable/> (signed drivers, anti-cheat safe):
   - VB-CABLE C+D → **Virtual Mic** (C) and **Game** (D). These run natively at 48 kHz with the smallest buffer,
     so they get the most delay-sensitive paths.
   - VB-CABLE A+B → **Chat** (A, Discord/TeamSpeak) and **Media** (B)
   - VB-CABLE (free) → **Aux**

   In each cable's control panel keep the buffer around 43 ms: Max Latency 2048 at 48 kHz internal rate, or
   4096 at 96 kHz (VB-CABLE and the A+B pack don't offer 2048 at 48 kHz). Smaller than the ~31 ms chunks the
   cables deliver in causes crackling; larger just adds delay.

   Reboot afterwards. Smowaudio auto-detects the cables on first launch; you can change the mapping in **Settings**.
2. *(Optional)* Rename the endpoints in Windows Sound settings, e.g. `CABLE-A Input` → `Game`, and `CABLE-C Output` → `Virtual Mic`.
   Matching uses the hardware name in parentheses, so renaming doesn't break anything.
3. In Windows Sound settings set **CABLE Input** (System) as the default output device. Everything not assigned to Game/Media
   then flows through Smowaudio. Your headphones are selected in Smowaudio's Settings tab, not as the Windows default.
4. In Discord/OBS/etc. pick **CABLE-C Output** (your Virtual Mic) as the microphone, and turn off their own noise suppression
   (Krisp, echo cancellation, auto gain) so the audio isn't processed twice.
5. Quit Sonar and Wave Link (or disable their startup). They grab default devices and will fight over routing.

## Build & run

Requirements: Rust (MSVC toolchain), Node.js, Visual Studio C++ build tools, WebView2 (built into Windows 11).

```bash
npm install
npm run dev      # debug build with the window open
npm run build    # optimized installer in src-tauri/target/release/bundle/nsis
```

Run the built exe with `--background` to start straight into the tray. The "Launch at Windows sign-in" toggle
registers a Task Scheduler task (`Smowaudio`, runs at your logon, no elevation needed) that does this for you.
Launching the exe while it's already running just opens the existing window.

```bash
cargo test --manifest-path src-tauri/Cargo.toml   # DSP unit tests
```

## Latency

| Path | Approx. latency |
| --- | --- |
| App → headphones | ~50 ms (cable + 30 ms drift buffer + output buffer) |
| Mic → Virtual Mic, noise removal on | ~90 ms (DeepFilterNet needs ~40 ms of look-ahead) |
| Mic → Virtual Mic, noise removal off | ~50 ms |

Fine for voice chat and streaming. For competitive-game audio latency, keep the game on your headphones directly
by leaving it on "Default" in the Apps tab and setting the headphones as the Windows default output.

## Layout

```
src-tauri/src/
  main.rs            tray, window, commands exposed to the UI
  engine.rs          supervised audio threads, settings hand-off, meters
  config.rs          %APPDATA%\Smowaudio\config.json, cable auto-detection
  audio/device.rs    endpoint enumeration
  audio/stream.rs    event-driven WASAPI capture/render (48 kHz float)
  audio/resample.rs  lock-free ring buffer + drift-compensating sinc resampler
  audio/routing.rs   per-app output device (AudioPolicyConfig) and session listing
  dsp/               EQ (biquads), gate, compressor, DeepFilterNet wrapper, chains
ui/                  plain HTML/CSS/JS (no bundler)
vendor/DeepFilterNet official libDF + DFN3 model with a small port to current tract (see VENDORED.md)
```
