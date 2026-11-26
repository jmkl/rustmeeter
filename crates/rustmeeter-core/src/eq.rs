//! Parametric EQ model shared by the audio engine, the CLI and any GUI.
//!
//! `EqSettings` is plain data: a bypass flag and up to `EQ_MAX_BANDS` bands, each with a filter
//! type, frequency, gain, Q and an on/off switch. The audio thread turns it into biquad filters
//! (`EqBand::coefficients`). A GUI can draw the combined curve with `EqSettings::response_db`
//! without knowing anything about filters.

use biquad::{Coefficients, ToHertz, Type};

pub const EQ_MAX_BANDS: usize = 10;
pub const EQ_FREQ_MIN: f32 = 20.0;
pub const EQ_FREQ_MAX: f32 = 20_000.0;
pub const EQ_GAIN_MIN: f32 = -15.0;
pub const EQ_GAIN_MAX: f32 = 15.0;
pub const EQ_Q_MIN: f32 = 0.1;
pub const EQ_Q_MAX: f32 = 10.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EqKind {
    HighPass,
    LowShelf,
    Bell,
    HighShelf,
    LowPass,
}

impl EqKind {
    pub const ALL: [EqKind; 5] = [
        EqKind::HighPass,
        EqKind::LowShelf,
        EqKind::Bell,
        EqKind::HighShelf,
        EqKind::LowPass,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqKind::HighPass => "High-Pass",
            EqKind::LowShelf => "Low-Shelf",
            EqKind::Bell => "Bell",
            EqKind::HighShelf => "High-Shelf",
            EqKind::LowPass => "Low-Pass",
        }
    }

    /// Short code used by the CLI: hp, ls, bell, hs, lp.
    pub fn code(self) -> &'static str {
        match self {
            EqKind::HighPass => "hp",
            EqKind::LowShelf => "ls",
            EqKind::Bell => "bell",
            EqKind::HighShelf => "hs",
            EqKind::LowPass => "lp",
        }
    }

    pub fn parse(s: &str) -> Option<EqKind> {
        match s.to_ascii_lowercase().as_str() {
            "hp" | "highpass" | "high-pass" => Some(EqKind::HighPass),
            "ls" | "lowshelf" | "low-shelf" => Some(EqKind::LowShelf),
            "bell" | "peak" | "peaking" => Some(EqKind::Bell),
            "hs" | "highshelf" | "high-shelf" => Some(EqKind::HighShelf),
            "lp" | "lowpass" | "low-pass" => Some(EqKind::LowPass),
            _ => None,
        }
    }

    /// Position in `EqKind::ALL` (handy for combo boxes).
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    /// High-pass and low-pass have no gain control.
    pub fn uses_gain(self) -> bool {
        matches!(self, EqKind::LowShelf | EqKind::Bell | EqKind::HighShelf)
    }

    pub fn default_q(self) -> f32 {
        match self {
            EqKind::Bell => 1.5,
            _ => 0.75,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EqBand {
    pub enabled: bool,
    pub kind: EqKind,
    /// Centre (or corner) frequency, in Hz.
    pub freq_hz: f32,
    /// Boost or cut, in dB. Ignored by the pass filters.
    pub gain_db: f32,
    pub q: f32,
}

fn finite_or(x: f32, default: f32) -> f32 {
    if x.is_finite() { x } else { default }
}

impl Default for EqBand {
    /// A neutral bell at 1 kHz: what an omitted field in a settings file becomes.
    fn default() -> Self {
        Self {
            enabled: true,
            kind: EqKind::Bell,
            freq_hz: 1000.0,
            gain_db: 0.0,
            q: EqKind::Bell.default_q(),
        }
    }
}

impl EqBand {
    pub fn new(kind: EqKind, freq_hz: f32, gain_db: f32) -> Self {
        Self {
            enabled: true,
            kind,
            freq_hz,
            gain_db,
            q: kind.default_q(),
        }
        .sanitized()
    }

    /// Clamp every field into its legal range (and zero the gain of kinds that have none).
    pub fn sanitized(mut self) -> Self {
        self.freq_hz = finite_or(self.freq_hz, 1000.0).clamp(EQ_FREQ_MIN, EQ_FREQ_MAX);
        self.gain_db = finite_or(self.gain_db, 0.0).clamp(EQ_GAIN_MIN, EQ_GAIN_MAX);
        self.q = finite_or(self.q, self.kind.default_q()).clamp(EQ_Q_MIN, EQ_Q_MAX);
        if !self.kind.uses_gain() {
            self.gain_db = 0.0;
        }
        self
    }

    /// Biquad coefficients for sample rate `fs`, or `None` if the parameters are unusable.
    pub(crate) fn coefficients(&self, fs: f32) -> Option<Coefficients<f32>> {
        let f0 = self
            .freq_hz
            .clamp(EQ_FREQ_MIN, (fs * 0.45).max(EQ_FREQ_MIN + 1.0));
        let q = self.q.clamp(EQ_Q_MIN, EQ_Q_MAX);
        let kind: Type<f32> = match self.kind {
            EqKind::HighPass => Type::HighPass,
            EqKind::LowPass => Type::LowPass,
            EqKind::LowShelf => Type::LowShelf(self.gain_db),
            EqKind::HighShelf => Type::HighShelf(self.gain_db),
            EqKind::Bell => Type::PeakingEQ(self.gain_db),
        };
        Coefficients::<f32>::from_params(kind, fs.hz(), f0.hz(), q).ok()
    }

    /// Magnitude response of this band alone, in dB, at `freq_hz` (0 if the band is off).
    pub fn response_db(&self, freq_hz: f32, fs: f32) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        let Some(c) = self.coefficients(fs) else {
            return 0.0;
        };
        let (b0, b1, b2) = (f64::from(c.b0), f64::from(c.b1), f64::from(c.b2));
        let (a1, a2) = (f64::from(c.a1), f64::from(c.a2));
        let w = 2.0 * std::f64::consts::PI * f64::from(freq_hz) / f64::from(fs);
        let (cw, c2w) = (w.cos(), (2.0 * w).cos());
        let num =
            b0 * b0 + b1 * b1 + b2 * b2 + 2.0 * (b0 * b1 + b1 * b2) * cw + 2.0 * b0 * b2 * c2w;
        let den = 1.0 + a1 * a1 + a2 * a2 + 2.0 * (a1 + a1 * a2) * cw + 2.0 * a2 * c2w;
        (10.0 * (num / den).max(1e-12).log10()) as f32
    }
}

#[derive(Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EqSettings {
    pub bypass: bool,
    pub bands: Vec<EqBand>,
}

impl EqSettings {
    /// Enforce the band limit and clamp every band. `Params::set_eq` does this for you.
    pub fn sanitize(&mut self) {
        self.bands.truncate(EQ_MAX_BANDS);
        for b in &mut self.bands {
            *b = b.sanitized();
        }
    }

    /// Append a band. Returns false (and does nothing) when `EQ_MAX_BANDS` is reached.
    pub fn add(&mut self, band: EqBand) -> bool {
        if self.bands.len() >= EQ_MAX_BANDS {
            return false;
        }
        self.bands.push(band.sanitized());
        true
    }

    /// Number of bands that are switched on.
    pub fn active_bands(&self) -> usize {
        self.bands.iter().filter(|b| b.enabled).count()
    }

    /// Combined response of all enabled bands in dB (0 when bypassed).
    pub fn response_db(&self, freq_hz: f32, fs: f32) -> f32 {
        if self.bypass {
            return 0.0;
        }
        self.bands.iter().map(|b| b.response_db(freq_hz, fs)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn empty_is_flat() {
        assert_eq!(EqSettings::default().response_db(1000.0, FS), 0.0);
    }

    #[test]
    fn bell_peaks_at_its_gain() {
        let mut s = EqSettings::default();
        s.add(EqBand::new(EqKind::Bell, 1000.0, 6.0));
        assert!(close(s.response_db(1000.0, FS), 6.0, 0.05));
        assert!(close(s.response_db(30.0, FS), 0.0, 0.2));
    }

    #[test]
    fn high_pass_cuts_lows_and_is_minus_3db_at_corner() {
        let mut band = EqBand::new(EqKind::HighPass, 100.0, 0.0);
        band.q = 0.7071;
        assert!(close(band.response_db(100.0, FS), -3.01, 0.1));
        assert!(band.response_db(10.0, FS) < -30.0);
        assert!(close(band.response_db(5000.0, FS), 0.0, 0.05));
    }

    #[test]
    fn low_pass_cuts_highs() {
        let mut band = EqBand::new(EqKind::LowPass, 5000.0, 0.0);
        band.q = 0.7071;
        assert!(close(band.response_db(5000.0, FS), -3.01, 0.1));
        assert!(band.response_db(20_000.0, FS) < -30.0);
    }

    #[test]
    fn shelves_reach_their_gain_far_from_the_corner() {
        let low = EqBand::new(EqKind::LowShelf, 200.0, 6.0);
        assert!(close(low.response_db(20.0, FS), 6.0, 0.7));
        assert!(close(low.response_db(10_000.0, FS), 0.0, 0.3));
        let high = EqBand::new(EqKind::HighShelf, 8000.0, 6.0);
        assert!(close(high.response_db(20_000.0, FS), 6.0, 0.7));
        assert!(close(high.response_db(100.0, FS), 0.0, 0.3));
    }

    #[test]
    fn disabled_band_and_bypass_are_flat() {
        let mut s = EqSettings::default();
        s.add(EqBand::new(EqKind::Bell, 1000.0, 6.0));
        s.bands[0].enabled = false;
        assert_eq!(s.response_db(1000.0, FS), 0.0);
        s.bands[0].enabled = true;
        s.bypass = true;
        assert_eq!(s.response_db(1000.0, FS), 0.0);
    }

    #[test]
    fn sanitize_clamps_and_limits() {
        let mut s = EqSettings::default();
        for _ in 0..(EQ_MAX_BANDS + 3) {
            s.bands.push(EqBand {
                enabled: true,
                kind: EqKind::Bell,
                freq_hz: f32::NAN,
                gain_db: 99.0,
                q: -1.0,
            });
        }
        s.sanitize();
        assert_eq!(s.bands.len(), EQ_MAX_BANDS);
        let b = s.bands[0];
        assert!(b.freq_hz >= EQ_FREQ_MIN && b.freq_hz <= EQ_FREQ_MAX);
        assert_eq!(b.gain_db, EQ_GAIN_MAX);
        assert_eq!(b.q, EQ_Q_MIN);
        assert!(!s.add(EqBand::new(EqKind::Bell, 1000.0, 0.0)));
    }

    #[test]
    fn pass_filters_have_no_gain() {
        assert_eq!(EqBand::new(EqKind::HighPass, 80.0, 9.0).gain_db, 0.0);
    }
}
