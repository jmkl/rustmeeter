//! Energy-based utterance segmenter for 16 kHz mono i16. Cheap, no model needed.
//! Emits a chunk after ~600 ms of silence (or at 25 s max), with 200 ms pre-roll.

use chrono::{DateTime, Local};
use std::collections::VecDeque;

const FRAME: usize = 320; // 20 ms
const PREROLL_FRAMES: usize = 10;
const HANGOVER_FRAMES: usize = 30;
const MIN_VOICED_FRAMES: usize = 10; // ignore blips shorter than 200 ms
const MAX_SAMPLES: usize = 16_000 * 25;

pub struct Utterance {
    pub samples: Vec<i16>, // 16 kHz mono
    pub started_at: DateTime<Local>,
}

pub struct Segmenter {
    carry: Vec<i16>,
    preroll: VecDeque<Vec<i16>>,
    cur: Vec<i16>,
    in_speech: bool,
    silence: usize,
    voiced: usize,
    floor_db: f32,
}

impl Default for Segmenter {
    fn default() -> Self {
        Self::new()
    }
}

fn rms_db(frame: &[i16]) -> f32 {
    let sum: f32 = frame.iter().map(|&s| (s as f32 / 32768.0).powi(2)).sum();
    20.0 * (sum / frame.len() as f32).sqrt().max(1e-6).log10()
}

impl Segmenter {
    pub fn new() -> Self {
        Self {
            carry: vec![],
            preroll: VecDeque::new(),
            cur: vec![],
            in_speech: false,
            silence: 0,
            voiced: 0,
            floor_db: -70.0,
        }
    }

    pub fn push(&mut self, samples: &[i16]) -> Vec<Utterance> {
        let mut out = vec![];
        self.carry.extend_from_slice(samples);
        while self.carry.len() >= FRAME {
            let frame: Vec<i16> = self.carry.drain(..FRAME).collect();
            self.process(frame, &mut out);
        }
        out
    }

    /// True between the first voiced frame and the end of the hangover: an utterance is being
    /// collected. Frontends use it to show "hearing speech" while the model is still idle.
    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    fn process(&mut self, frame: Vec<i16>, out: &mut Vec<Utterance>) {
        let db = rms_db(&frame);
        let thr = (self.floor_db + 12.0).max(-50.0);
        let voiced = db > thr;
        if !voiced && !self.in_speech {
            self.floor_db = (0.98 * self.floor_db + 0.02 * db).clamp(-80.0, -35.0);
        }
        if self.in_speech {
            self.cur.extend_from_slice(&frame);
            if voiced {
                self.silence = 0;
                self.voiced += 1;
            } else {
                self.silence += 1;
            }
            if self.silence >= HANGOVER_FRAMES {
                self.finish(out, true);
            } else if self.cur.len() >= MAX_SAMPLES {
                self.finish(out, false);
            }
        } else if voiced {
            self.in_speech = true;
            self.cur.clear();
            for f in self.preroll.drain(..) {
                self.cur.extend_from_slice(&f);
            }
            self.cur.extend_from_slice(&frame);
            self.silence = 0;
            self.voiced = 1;
        } else {
            self.preroll.push_back(frame);
            if self.preroll.len() > PREROLL_FRAMES {
                self.preroll.pop_front();
            }
        }
    }

    fn finish(&mut self, out: &mut Vec<Utterance>, by_silence: bool) {
        if self.voiced >= MIN_VOICED_FRAMES {
            if by_silence {
                let trim = self.silence.saturating_sub(10) * FRAME;
                let keep = self.cur.len().saturating_sub(trim);
                self.cur.truncate(keep);
            }
            let samples = std::mem::take(&mut self.cur);
            let started_at =
                Local::now() - chrono::Duration::milliseconds((samples.len() / 16) as i64);
            out.push(Utterance {
                samples,
                started_at,
            });
        } else {
            self.cur.clear();
        }
        self.in_speech = false;
        self.silence = 0;
        self.voiced = 0;
    }
}
