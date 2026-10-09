//! The EQ editor's logic (the Mixer's drawer and the Mic tab use it): presets, the response curve
//! drawn as a path, and the edits made by dragging and scrolling over its points. The graph is
//! described in a viewbox 1000 units wide (x = 20 Hz..20 kHz on a log scale) and as tall as the
//! graph is in logical pixels, so labels and points keep their size at any width.

use crate::dsp::biquad::{BandKind, EqBand};
use crate::dsp::chain::EqSettings;
use crate::ui::{EqBandData, EqData};

/// ±15 dB fills the graph, less a 12px margin at the top and bottom.
const RANGE: f32 = 15.0;
const MARGIN: f32 = 12.0;
pub const VIEW_W: f32 = 1000.0;

fn band(kind: BandKind, freq: f32, gain_db: f32, q: f32) -> EqBand {
    EqBand { kind, freq, gain_db, q, enabled: true }
}

/// The bands of a preset (same as the old page's).
pub fn preset(name: &str) -> Option<Vec<EqBand>> {
    use BandKind::*;
    Some(match name {
        "Flat" => vec![band(LowShelf, 100.0, 0.0, 0.707), band(Peaking, 400.0, 0.0, 1.0), band(Peaking, 1500.0, 0.0, 1.0), band(Peaking, 4000.0, 0.0, 1.0), band(HighShelf, 10000.0, 0.0, 0.707)],
        "Footsteps" => vec![band(LowShelf, 110.0, -4.0, 0.707), band(Peaking, 260.0, -2.0, 1.0), band(Peaking, 2200.0, 4.0, 1.2), band(Peaking, 4800.0, 5.0, 1.4), band(HighShelf, 11000.0, -2.0, 0.707)],
        "Bass boost" => vec![band(LowShelf, 90.0, 6.0, 0.707), band(Peaking, 420.0, -1.5, 1.0), band(Peaking, 1500.0, 0.0, 1.0), band(Peaking, 4000.0, 0.0, 1.0), band(HighShelf, 10000.0, 0.0, 0.707)],
        "Voice clarity" => vec![band(HighPass, 90.0, 0.0, 0.707), band(Peaking, 320.0, -3.0, 1.0), band(Peaking, 1500.0, 0.0, 1.0), band(Peaking, 3200.0, 4.0, 1.0), band(HighShelf, 9000.0, 2.0, 0.707)],
        "Night" => vec![band(LowShelf, 120.0, -5.0, 0.707), band(Peaking, 400.0, 0.0, 1.0), band(Peaking, 1500.0, 0.0, 1.0), band(Peaking, 4000.0, 0.0, 1.0), band(HighShelf, 8000.0, -3.0, 0.707)],
        _ => return None,
    })
}

fn has_gain(b: &EqBand) -> bool {
    !matches!(b.kind, BandKind::LowPass | BandKind::HighPass)
}

fn kind_label(kind: BandKind) -> &'static str {
    match kind {
        BandKind::LowShelf => "Low shelf",
        BandKind::HighShelf => "High shelf",
        BandKind::Peaking => "Peak",
        BandKind::LowPass => "High cut",
        BandKind::HighPass => "Low cut",
    }
}

/// x in the viewbox for a frequency, and back.
pub fn freq_x(f: f32) -> f32 {
    (f / 20.0).log10() / 3.0 * VIEW_W
}
fn x_freq(x: f32) -> f32 {
    20.0 * 10f32.powf(x / VIEW_W * 3.0)
}
/// y in logical pixels for a gain, in a graph `h` pixels tall, and back.
pub fn gain_y(g: f32, h: f32) -> f32 {
    h / 2.0 - g / RANGE * (h / 2.0 - MARGIN)
}
fn y_gain(y: f32, h: f32) -> f32 {
    (h / 2.0 - y) / (h / 2.0 - MARGIN) * RANGE
}

/// One band's response in dB at frequency `f` (the same RBJ formulas as the filters).
fn biquad_db(b: &EqBand, f: f32) -> f32 {
    if !b.enabled {
        return 0.0;
    }
    let fs = 48000.0f64;
    let w0 = 2.0 * std::f64::consts::PI * (b.freq as f64).min(fs * 0.49) / fs;
    let a = 10f64.powf(b.gain_db as f64 / 40.0);
    let (cos, sin) = (w0.cos(), w0.sin());
    let alpha = sin / (2.0 * (b.q as f64).max(0.05));
    let sa = 2.0 * a.sqrt() * alpha;
    let (b0, b1, b2, a0, a1, a2) = match b.kind {
        BandKind::LowShelf => (
            a * (a + 1.0 - (a - 1.0) * cos + sa),
            2.0 * a * (a - 1.0 - (a + 1.0) * cos),
            a * (a + 1.0 - (a - 1.0) * cos - sa),
            a + 1.0 + (a - 1.0) * cos + sa,
            -2.0 * (a - 1.0 + (a + 1.0) * cos),
            a + 1.0 + (a - 1.0) * cos - sa,
        ),
        BandKind::HighShelf => (
            a * (a + 1.0 + (a - 1.0) * cos + sa),
            -2.0 * a * (a - 1.0 + (a + 1.0) * cos),
            a * (a + 1.0 + (a - 1.0) * cos - sa),
            a + 1.0 - (a - 1.0) * cos + sa,
            2.0 * (a - 1.0 - (a + 1.0) * cos),
            a + 1.0 - (a - 1.0) * cos - sa,
        ),
        BandKind::LowPass => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        BandKind::HighPass => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        BandKind::Peaking => (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a),
    };
    let w = 2.0 * std::f64::consts::PI * f as f64 / fs;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let num = (b0 + b1 * c1 + b2 * c2).powi(2) + (b1 * s1 + b2 * s2).powi(2);
    let den = (a0 + a1 * c1 + a2 * c2).powi(2) + (a1 * s1 + a2 * s2).powi(2);
    (10.0 * (num / den).log10()) as f32
}

fn fmt_freq(f: f32) -> String {
    if f >= 1000.0 {
        if (f % 1000.0).abs() < 0.5 {
            format!("{:.0} kHz", f / 1000.0)
        } else {
            format!("{:.1} kHz", f / 1000.0)
        }
    } else {
        format!("{} Hz", f.round())
    }
}

fn fmt_gain(b: &EqBand) -> String {
    if !has_gain(b) {
        return "12 dB/oct".into();
    }
    let sign = if b.gain_db > 0.0 { "+" } else if b.gain_db < 0.0 { "−" } else { "" };
    format!("{sign}{:.1} dB", b.gain_db.abs())
}

/// What the editor shows for these settings, in a graph `h` logical pixels tall.
pub fn view(eq: &EqSettings, h: f32) -> EqData {
    // The curve every 4 viewbox units, kept inside the graph.
    let points: Vec<(f32, f32)> = (0..=250)
        .map(|i| {
            let x = i as f32 * 4.0;
            let f = x_freq(x);
            let g: f32 = eq.bands.iter().map(|b| biquad_db(b, f)).sum();
            (x, gain_y(g, h).clamp(2.0, h - 2.0))
        })
        .collect();
    let mut curve = String::new();
    for (i, (x, y)) in points.iter().enumerate() {
        curve += &format!("{}{x:.1} {y:.2} ", if i == 0 { "M" } else { "L" });
    }
    // The area between the curve and 0 dB, filled faintly.
    let zero = gain_y(0.0, h);
    let area = format!("{curve}L{VIEW_W} {zero:.2} L0 {zero:.2} Z");
    let bands: Vec<EqBandData> = eq
        .bands
        .iter()
        .map(|b| EqBandData {
            kind: kind_label(b.kind).into(),
            value: format!("{} · {}", fmt_freq(b.freq), fmt_gain(b)).into(),
            x: freq_x(b.freq) / VIEW_W,
            y: gain_y(if has_gain(b) { b.gain_db } else { 0.0 }, h),
            on: b.enabled,
        })
        .collect();
    EqData {
        enabled: eq.enabled,
        preset: eq.preset.as_str().into(),
        curve: curve.into(),
        area: area.into(),
        bands: std::rc::Rc::new(slint::VecModel::from(bands)).into(),
        height: h,
    }
}

/// The band whose point is within `radius` pixels of (x, y) in a graph `w`×`h`, if any.
pub fn pick(eq: &EqSettings, x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<usize> {
    let mut best = None;
    let mut best_d = radius;
    for (i, b) in eq.bands.iter().enumerate() {
        let bx = freq_x(b.freq) / VIEW_W * w;
        let by = gain_y(if has_gain(b) { b.gain_db } else { 0.0 }, h);
        let d = (bx - x).hypot(by - y);
        if d < best_d {
            best = Some(i);
            best_d = d;
        }
    }
    best
}

/// Moves band `i` to (x, y) in a graph `w`×`h`: frequency from x, gain from y (in 0.5 dB steps).
/// The EQ turns on and its preset becomes "Custom".
pub fn drag(eq: &mut EqSettings, i: usize, x: f32, y: f32, w: f32, h: f32) {
    let Some(b) = eq.bands.get_mut(i) else { return };
    b.freq = x_freq(x / w * VIEW_W).clamp(20.0, 20000.0).round();
    if has_gain(b) {
        b.gain_db = (y_gain(y, h).clamp(-RANGE, RANGE) * 2.0).round() / 2.0;
    }
    eq.preset = "Custom".into();
    eq.enabled = true;
}

/// Makes band `i` narrower (scrolling up) or wider: Q ×1.1 or ×0.9, 0.1..10.
pub fn widen(eq: &mut EqSettings, i: usize, narrower: bool) {
    let Some(b) = eq.bands.get_mut(i) else { return };
    b.q = ((b.q * if narrower { 1.1 } else { 0.9 }).clamp(0.1, 10.0) * 100.0).round() / 100.0;
    eq.preset = "Custom".into();
}

/// Applies a preset: its bands, its name, and the EQ on.
pub fn apply_preset(eq: &mut EqSettings, name: &str) {
    if let Some(bands) = preset(name) {
        eq.bands = bands;
        eq.preset = name.into();
        eq.enabled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_is_flat_and_boosts_show_up() {
        let mut eq = EqSettings { enabled: true, preset: "Flat".into(), bands: preset("Flat").unwrap() };
        for f in [30.0, 200.0, 1000.0, 8000.0] {
            let g: f32 = eq.bands.iter().map(|b| biquad_db(b, f)).sum();
            assert!(g.abs() < 0.01, "{f} Hz: {g} dB");
        }
        apply_preset(&mut eq, "Bass boost");
        let low: f32 = eq.bands.iter().map(|b| biquad_db(b, 40.0)).sum();
        assert!((low - 6.0).abs() < 0.5, "a 6 dB low shelf gives about +6 dB at 40 Hz, got {low}");
    }

    #[test]
    fn dragging_a_point_moves_its_band_and_makes_it_custom() {
        let mut eq = EqSettings { enabled: false, preset: "Flat".into(), bands: preset("Flat").unwrap() };
        let (w, h) = (500.0, 118.0);
        let x = freq_x(1500.0) / VIEW_W * w;
        assert_eq!(pick(&eq, x + 3.0, gain_y(0.0, h) - 2.0, w, h, 16.0), Some(2));
        drag(&mut eq, 2, freq_x(2000.0) / VIEW_W * w, gain_y(6.0, h), w, h);
        assert_eq!((eq.bands[2].freq, eq.bands[2].gain_db), (2000.0, 6.0));
        assert!(eq.enabled && eq.preset == "Custom");
    }
}
