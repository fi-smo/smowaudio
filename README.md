# Smowaudio

A lightweight replacement for SteelSeries Sonar / Elgato Wave Link / Voicemeeter on Windows 11.

- **Output channels:** Game, Chat, Media, Aux (the same as SteelSeries Sonar), each with volume, mute, and an optional
  parametric EQ, mixed to your headphones. Game is your Windows default output, so it also carries system sounds.
- **Virtual Mic:** Noise Removal (DeepFilterNet3 AI) → Noise Gate → Equalizer → Compressor → output gain.
- **Per-app routing:** drag any app onto a channel in the Apps tab. Windows remembers it.
- **Tray flyout:** left-click the tray icon for channel volumes and mutes, the output device, and mic mute/listen.
- **Keyboard shortcuts:** bind almost everything in Settings (volumes, mutes, every mic filter, push to talk,
  output device). Nothing is bound by default. A small overlay in the top-right corner confirms each change.
- **Mic test:** record 5 seconds in the Mic tab and play it back filtered or untouched.
- **A master for each device:** headphones and speakers each keep their own master volume and EQ.
- **Updates:** installed automatically while nothing is playing (or with one click), with a changelog in Settings.
- **Starts fast:** the audio engine starts before the UI. Launched at sign-in, it sits in the tray and the WebView window is only created when you open it.

## Planned

- **Ducking:** turn Game and Media down while someone talks on Chat, and back up afterwards.
- **Per-app volume and mute** on the Apps cards, like the Windows volume mixer.
- **ChatMix:** one slider (and shortcut) that balances Game against Chat, like the dial on SteelSeries headsets.

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
3. Like Sonar, Smowaudio makes Game the Windows default output, Chat the communications output and the Virtual Mic
   the default recording device (Settings → "Set Windows default devices"; turning it off restores your own).
   Pick your headphones in the tray flyout or in Settings, not as the Windows default.
4. In Discord/OBS/etc. pick the Virtual Mic (**CABLE-C Output**) as the microphone, and turn off their own noise
   suppression (Krisp, echo cancellation, auto gain) so the audio isn't processed twice.
5. Quit Sonar and Wave Link (or disable their startup). They grab default devices and will fight over routing.

## Build & run

Requirements: Rust (MSVC toolchain), Node.js, Visual Studio C++ build tools, WebView2 (built into Windows 11).

```bash
npm install
npm run dev      # debug build with the window open
npm run build    # optimized src-tauri/target/release/smowaudio.exe (no installer)
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
| App → headphones | cable delay (~30–45 ms) + 10–20 ms engine buffer + output buffer; ~80 ms measured cable to cable |
| Mic → Virtual Mic | as above, plus 30 ms for noise removal (10 ms with the low-latency model) |

Fine for voice chat and streaming. For competitive-game audio latency, keep the game on your headphones directly
by leaving it on "Default" in the Apps tab and setting the headphones as the Windows default output.

## Layout

```
src-tauri/src/
  main.rs            tray, windows, commands exposed to the UI
  hotkeys.rs         global keyboard shortcuts
  osd.rs             top-right overlay shown by shortcuts
  icons.rs           app icons for the Apps tab
  engine.rs          supervised audio threads, settings hand-off, meters
  config.rs          %APPDATA%\Smowaudio\config.json, cable auto-detection
  audio/device.rs    endpoint enumeration
  audio/stream.rs    event-driven WASAPI capture/render (48 kHz float)
  audio/resample.rs  lock-free ring buffer + drift-compensating sinc resampler
  audio/routing.rs   per-app output device (AudioPolicyConfig) and session listing
  dsp/               EQ (biquads), gate, compressor, DeepFilterNet wrapper, chains
ui/                  plain HTML/CSS/JS (no bundler)
  app/*.js           main window, one script per view (core, mixer, apps, mic, settings, shortcuts, main)
vendor/DeepFilterNet official libDF + DFN3 model with a small port to current tract (see VENDORED.md)
```
