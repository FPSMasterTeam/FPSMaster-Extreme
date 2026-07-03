//! Lightweight per-frame CPU phase timing, to pin down frame-time spikes (e.g.
//! while chunks stream in and the camera turns). Measurement always runs (it's
//! near-free) so the F3 overlay can read [`FrameProfiler::latest`] live; only the
//! *log* output is gated on the `frame_profile` target — pass `--profile-frames`,
//! or set `RUST_LOG=frame_profile=debug`.
//!
//! Log output: a once-a-second summary (per-phase avg/max ms over the window) at
//! debug, plus a one-line breakdown at warn for any single frame whose CPU work
//! spikes well above the running average. To profile a new phase, time it with
//! `Instant::now()` and call `profiler.record("name", t.elapsed())`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// A snapshot of the most recent ~1s window, for live display (the F3 overlay).
#[derive(Debug, Clone, Default)]
pub struct PhaseSummary {
    pub frames: u32,
    pub cpu_avg_ms: f32,
    pub cpu_max_ms: f32,
    /// `(phase, avg ms, max ms)` over the window, sorted by phase name.
    pub phases: Vec<(&'static str, f32, f32)>,
}

pub struct FrameProfiler {
    /// Whether the `frame_profile` log target is enabled (controls logging only).
    on: bool,
    /// This frame's per-phase durations (one entry per phase, overwritten).
    cur: BTreeMap<&'static str, Duration>,
    /// Per-phase sum/max accumulated over the current ~1s window.
    sum: BTreeMap<&'static str, Duration>,
    max: BTreeMap<&'static str, Duration>,
    frames: u32,
    frame_sum: Duration,
    frame_max: Duration,
    last_flush: Instant,
    /// Smoothed CPU frame time (ms), for relative spike detection.
    ema_ms: f32,
    /// Last completed window summary, served to the F3 overlay.
    latest: PhaseSummary,
}

impl FrameProfiler {
    pub fn new(now: Instant) -> Self {
        Self {
            on: false,
            cur: BTreeMap::new(),
            sum: BTreeMap::new(),
            max: BTreeMap::new(),
            frames: 0,
            frame_sum: Duration::ZERO,
            frame_max: Duration::ZERO,
            last_flush: now,
            ema_ms: 0.0,
            latest: PhaseSummary::default(),
        }
    }

    /// The most recent ~1s window summary (updated once a second), for the F3
    /// overlay. Empty until the first window completes.
    pub fn latest(&self) -> &PhaseSummary {
        &self.latest
    }

    /// Record one phase's duration for the current frame. Overwrites, so each
    /// phase is recorded at most once per frame. Always accumulated (cheap) so the
    /// F3 readout works even without the log flag.
    pub fn record(&mut self, phase: &'static str, dur: Duration) {
        self.cur.insert(phase, dur);
    }

    /// Close out the current frame: roll the phase timings into the window, refresh
    /// the live summary every second, and (when the log target is on) report a spike
    /// if this frame's CPU work ran long. `frame_dt` is the wall-clock frame interval.
    pub fn end_frame(&mut self, now: Instant, frame_dt: Duration) {
        self.on = log::log_enabled!(target: "frame_profile", log::Level::Debug);

        // CPU total = sum of the measured phases (excludes the vsync sleep that
        // frame_dt carries), so the spike test keys off real work.
        let cpu: Duration = self.cur.values().copied().sum();
        let cpu_ms = cpu.as_secs_f32() * 1000.0;
        let avg = if self.ema_ms > 0.0 { self.ema_ms } else { cpu_ms };

        if self.on && cpu_ms > avg * 2.0 && cpu_ms > 6.0 {
            log::warn!(
                target: "frame_profile",
                "SPIKE {cpu_ms:.1}ms cpu (avg {avg:.1}ms, frame {:.1}ms):{}",
                frame_dt.as_secs_f32() * 1000.0,
                Self::breakdown(&self.cur),
            );
        }
        self.ema_ms = if self.ema_ms > 0.0 {
            self.ema_ms * 0.9 + cpu_ms * 0.1
        } else {
            cpu_ms
        };

        for (k, v) in &self.cur {
            *self.sum.entry(k).or_default() += *v;
            let m = self.max.entry(k).or_default();
            *m = (*m).max(*v);
        }
        self.frame_sum += cpu;
        self.frame_max = self.frame_max.max(cpu);
        self.frames += 1;
        self.cur.clear();

        if now.duration_since(self.last_flush) >= Duration::from_secs(1) {
            let n = self.frames.max(1);
            let phases: Vec<(&'static str, f32, f32)> = self
                .sum
                .iter()
                .map(|(k, total)| {
                    (
                        *k,
                        total.as_secs_f32() * 1000.0 / n as f32,
                        self.max.get(k).copied().unwrap_or_default().as_secs_f32() * 1000.0,
                    )
                })
                .collect();
            self.latest = PhaseSummary {
                frames: self.frames,
                cpu_avg_ms: self.frame_sum.as_secs_f32() * 1000.0 / n as f32,
                cpu_max_ms: self.frame_max.as_secs_f32() * 1000.0,
                phases,
            };
            if self.on {
                let mut s = String::new();
                for (k, a, mx) in &self.latest.phases {
                    s.push_str(&format!(" {k} {a:.2}/{mx:.2}"));
                }
                log::debug!(
                    target: "frame_profile",
                    "{} frames | cpu avg/max {:.2}/{:.2}ms | phases avg/max ms:{s}",
                    self.latest.frames,
                    self.latest.cpu_avg_ms,
                    self.latest.cpu_max_ms,
                );
            }
            self.sum.clear();
            self.max.clear();
            self.frames = 0;
            self.frame_sum = Duration::ZERO;
            self.frame_max = Duration::ZERO;
            self.last_flush = now;
        }
    }

    fn breakdown(cur: &BTreeMap<&'static str, Duration>) -> String {
        let mut s = String::new();
        for (k, v) in cur {
            s.push_str(&format!(" {k}={:.2}ms", v.as_secs_f32() * 1000.0));
        }
        s
    }
}
