//! Low-level audio engine (internal; use `Mixer` instead): N input strips -> per-strip parametric EQ/gain/mute -> 2 output buses (A, B).
//! Inputs can be real mics or WASAPI loopback (any output device used as a source).

use crate::devices::{InputSource, find_input, find_output};
use crate::eq::EQ_MAX_BANDS;
use crate::events::{Event, EventHandler};
use crate::params::{NUM_BUSES, Params, db_to_lin};
use crate::tap::TapShared;
use anyhow::{Result, anyhow, bail};
use biquad::{Biquad, Coefficients, DirectForm2Transposed};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

// ---------- EQ stages (built from `EqSettings` on the output thread) ----------

struct EqStage {
    enabled: bool,
    filt: [DirectForm2Transposed<f32>; 2], // left, right
}

fn identity() -> Coefficients<f32> {
    Coefficients {
        a1: 0.0,
        a2: 0.0,
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
    }
}

// ---------- per (strip, bus) reader: resample + EQ + gain, runs in output callback ----------

struct Reader {
    cons: HeapConsumer<f32>, // interleaved stereo at input rate
    params: Arc<Params>,
    in_rate: f32,
    out_rate: f32,
    ratio: f64, // in_rate / out_rate
    pos: f64,
    prev: [f32; 2],
    cur: [f32; 2],
    eq: Vec<EqStage>,
    eq_bypass: bool,
    eq_version: u32,
    meter: bool,
}

impl Reader {
    fn new(
        cons: HeapConsumer<f32>,
        params: Arc<Params>,
        in_rate: f32,
        out_rate: f32,
        meter: bool,
    ) -> Self {
        Self {
            cons,
            params,
            in_rate,
            out_rate,
            ratio: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            prev: [0.0; 2],
            cur: [0.0; 2],
            eq: Vec::with_capacity(EQ_MAX_BANDS), // no allocation on the audio thread later
            eq_bypass: false,
            eq_version: u32::MAX, // force a load on the first buffer
            meter,
        }
    }

    /// Rebuild the filter chain when the strip's EQ changed. Keeps filter state for bands that
    /// stay in place (no clicks while dragging a dot) and never blocks the audio thread.
    fn refresh_eq(&mut self) {
        if self.params.eq_version() == self.eq_version {
            return;
        }
        let Some((version, settings)) = self.params.try_eq() else {
            return; // a writer holds the lock this instant: retry next buffer
        };
        self.eq_version = version;
        self.eq_bypass = settings.bypass;
        let fs = self.out_rate;
        for (i, band) in settings.bands.iter().enumerate() {
            let enabled = band.enabled && band.coefficients(fs).is_some();
            let co = || band.coefficients(fs).unwrap_or_else(identity);
            if let Some(stage) = self.eq.get_mut(i) {
                stage.enabled = enabled;
                stage.filt[0].update_coefficients(co());
                stage.filt[1].update_coefficients(co());
            } else {
                self.eq.push(EqStage {
                    enabled,
                    filt: [
                        DirectForm2Transposed::<f32>::new(co()),
                        DirectForm2Transposed::<f32>::new(co()),
                    ],
                });
            }
        }
        self.eq.truncate(settings.bands.len());
    }

    fn mix_into(&mut self, out: &mut [f32], frames: usize) {
        self.refresh_eq();
        let g = if self.params.mute.load(Relaxed) {
            0.0
        } else {
            db_to_lin(self.params.gain_db.get())
        };

        // Clock-drift control: if we're buffering >250 ms, drop down to ~50 ms.
        let max_fill = (self.in_rate * 0.25) as usize * 2;
        let target = (self.in_rate * 0.05) as usize * 2;
        if self.cons.len() > max_fill {
            while self.cons.len() > target && self.cons.len() >= 2 {
                self.cons.pop();
                self.cons.pop();
            }
        }

        let mut peak = 0f32;
        for f in 0..frames {
            self.pos += self.ratio;
            while self.pos >= 1.0 {
                self.pos -= 1.0;
                self.prev = self.cur;
                self.cur = if self.cons.len() >= 2 {
                    [
                        self.cons.pop().unwrap_or(0.0),
                        self.cons.pop().unwrap_or(0.0),
                    ]
                } else {
                    [0.0; 2] // underrun -> silence
                };
            }
            let t = self.pos as f32;
            for c in 0..2 {
                let mut x = self.prev[c] + (self.cur[c] - self.prev[c]) * t;
                if !self.eq_bypass {
                    for stage in self.eq.iter_mut() {
                        if stage.enabled {
                            x = stage.filt[c].run(x);
                        }
                    }
                }
                x *= g;
                peak = peak.max(x.abs());
                out[f * 2 + c] += x;
            }
        }
        if self.meter {
            let old = self.params.level.get();
            self.params.level.set(peak.max(old * 0.85));
        }
    }
}

struct BusMixer {
    readers: Vec<Reader>,
    params: Arc<Params>,
    scratch: Vec<f32>,
}

impl BusMixer {
    fn render(&mut self, frames: usize) {
        self.scratch.clear();
        self.scratch.resize(frames * 2, 0.0);
        for r in self.readers.iter_mut() {
            r.mix_into(&mut self.scratch, frames);
        }
        let g = if self.params.mute.load(Relaxed) {
            0.0
        } else {
            db_to_lin(self.params.gain_db.get())
        };
        let mut peak = 0f32;
        for s in self.scratch.iter_mut() {
            *s = (*s * g).clamp(-1.0, 1.0);
            peak = peak.max(s.abs());
        }
        let old = self.params.level.get();
        self.params.level.set(peak.max(old * 0.85));
    }
}

// ---------- stream builders ----------

fn build_input<T>(
    dev: &cpal::Device,
    cfg: &StreamConfig,
    mut prods: Vec<(usize, HeapProducer<f32>)>,
    params: Arc<Params>,
    tap: Option<Arc<TapShared>>,
    label: String,
    handler: EventHandler,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = cfg.channels as usize;
    let s = dev.build_input_stream(
        cfg,
        move |data: &[T], _| {
            // Raw mono copy for the tap (only while a Tap is subscribed; never blocks).
            let mut tap_guard = tap
                .as_ref()
                .filter(|t| t.enabled.load(Relaxed))
                .and_then(|t| t.prod.try_lock().ok());
            for frame in data.chunks(ch) {
                let l = f32::from_sample_(frame[0]);
                let r = if ch > 1 {
                    f32::from_sample_(frame[1])
                } else {
                    l
                };
                if let Some(g) = tap_guard.as_mut() {
                    let _ = g.push((l + r) * 0.5);
                }
                for (bus, p) in prods.iter_mut() {
                    if params.routes[*bus].load(Relaxed) {
                        let _ = p.push(l);
                        let _ = p.push(r);
                    }
                }
            }
        },
        move |e| (*handler)(&Event::Error(format!("input '{label}': {e}"))),
        None,
    )?;
    Ok(s)
}

fn build_output<T>(
    dev: &cpal::Device,
    cfg: &StreamConfig,
    mut mixer: BusMixer,
    label: String,
    handler: EventHandler,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = cfg.channels as usize;
    let s = dev.build_output_stream(
        cfg,
        move |data: &mut [T], _| {
            let frames = data.len() / ch;
            mixer.render(frames);
            for (i, frame) in data.chunks_mut(ch).enumerate() {
                let l = mixer.scratch[i * 2];
                let r = mixer.scratch[i * 2 + 1];
                if ch == 1 {
                    frame[0] = T::from_sample((l + r) * 0.5);
                } else {
                    frame[0] = T::from_sample(l);
                    frame[1] = T::from_sample(r);
                    for x in frame[2..].iter_mut() {
                        *x = T::from_sample(0.0f32);
                    }
                }
            }
        },
        move |e| (*handler)(&Event::Error(format!("{label}: {e}"))),
        None,
    )?;
    Ok(s)
}

// ---------- public engine ----------

pub struct StripSpec {
    pub source: InputSource,
    pub params: Arc<Params>,
    pub tap: Option<Arc<TapShared>>,
}

pub struct BusSpec {
    pub device: Option<String>,
    pub params: Arc<Params>,
}

/// Holds the live cpal streams. Drop it to stop all audio.
pub struct Engine {
    _streams: Vec<Stream>,
}

impl Engine {
    pub fn start(
        strips: &[StripSpec],
        buses: &[BusSpec; NUM_BUSES],
        handler: &EventHandler,
    ) -> Result<Engine> {
        let host = cpal::default_host();

        // Output devices first so we know their sample rates.
        let mut outs: Vec<Option<(cpal::Device, cpal::SupportedStreamConfig)>> = vec![];
        for b in buses.iter() {
            match &b.device {
                Some(name) => {
                    let d = find_output(&host, name)
                        .ok_or_else(|| anyhow!("output '{name}' not found"))?;
                    let c = d.default_output_config()?;
                    outs.push(Some((d, c)));
                }
                None => outs.push(None),
            }
        }
        let first_bus = outs.iter().position(|o| o.is_some());

        let mut readers: Vec<Vec<Reader>> = (0..NUM_BUSES).map(|_| vec![]).collect();
        let mut streams: Vec<Stream> = vec![];

        for s in strips {
            let (dev, cfg) = if s.source.loopback {
                let d = find_output(&host, &s.source.name)
                    .ok_or_else(|| anyhow!("device '{}' not found", s.source.name))?;
                let c = d.default_output_config()?;
                (d, c)
            } else {
                let d = find_input(&host, &s.source.name)
                    .ok_or_else(|| anyhow!("input '{}' not found", s.source.name))?;
                let c = d.default_input_config()?;
                (d, c)
            };
            let in_rate = cfg.sample_rate().0 as f32;
            let tap = s.tap.clone();
            if let Some(t) = &tap {
                t.rate.store(in_rate as u32, Relaxed);
            }

            let mut prods = vec![];
            for (bi, o) in outs.iter().enumerate() {
                if let Some((_, oc)) = o {
                    let rb = HeapRb::<f32>::new(in_rate as usize); // ~0.5 s stereo
                    let (p, c) = rb.split();
                    prods.push((bi, p));
                    readers[bi].push(Reader::new(
                        c,
                        s.params.clone(),
                        in_rate,
                        oc.sample_rate().0 as f32,
                        Some(bi) == first_bus,
                    ));
                }
            }

            let sc: StreamConfig = cfg.config();
            let p = s.params.clone();
            let stream = match cfg.sample_format() {
                SampleFormat::F32 => build_input::<f32>(
                    &dev,
                    &sc,
                    prods,
                    p,
                    tap.clone(),
                    s.source.label(),
                    handler.clone(),
                )?,
                SampleFormat::I16 => build_input::<i16>(
                    &dev,
                    &sc,
                    prods,
                    p,
                    tap.clone(),
                    s.source.label(),
                    handler.clone(),
                )?,
                SampleFormat::U16 => build_input::<u16>(
                    &dev,
                    &sc,
                    prods,
                    p,
                    tap.clone(),
                    s.source.label(),
                    handler.clone(),
                )?,
                SampleFormat::I32 => build_input::<i32>(
                    &dev,
                    &sc,
                    prods,
                    p,
                    tap.clone(),
                    s.source.label(),
                    handler.clone(),
                )?,
                f => bail!("unsupported input sample format {f:?}"),
            };
            stream.play()?;
            streams.push(stream);
        }

        for (bi, o) in outs.into_iter().enumerate() {
            if let Some((dev, cfg)) = o {
                let bus_label = format!("bus {}", if bi == 0 { "A" } else { "B" });
                let mixer = BusMixer {
                    readers: std::mem::take(&mut readers[bi]),
                    params: buses[bi].params.clone(),
                    scratch: vec![],
                };
                let sc: StreamConfig = cfg.config();
                let stream = match cfg.sample_format() {
                    SampleFormat::F32 => {
                        build_output::<f32>(&dev, &sc, mixer, bus_label, handler.clone())?
                    }
                    SampleFormat::I16 => {
                        build_output::<i16>(&dev, &sc, mixer, bus_label, handler.clone())?
                    }
                    SampleFormat::U16 => {
                        build_output::<u16>(&dev, &sc, mixer, bus_label, handler.clone())?
                    }
                    SampleFormat::I32 => {
                        build_output::<i32>(&dev, &sc, mixer, bus_label, handler.clone())?
                    }
                    f => bail!("unsupported output sample format {f:?}"),
                };
                stream.play()?;
                streams.push(stream);
            }
        }

        Ok(Engine { _streams: streams })
    }
}
