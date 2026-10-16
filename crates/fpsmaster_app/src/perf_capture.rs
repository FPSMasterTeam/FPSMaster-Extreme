//! Interactive render profiler, toggled in-game with **F10**.
//!
//! Press F10 once to begin sampling every rendered frame; press it again to
//! stop. On stop it prints a statistical summary to the log (frame-time
//! percentiles, GPU/CPU pass costs, the per-subsystem CPU breakdown and draw
//! counts) and writes the raw per-frame rows to a `perf_capture_<ts>.csv` so the
//! numbers can be graphed / diffed offline while optimizing.
//!
//! Unlike the `--profile-frames` log target (a passive once-a-second summary)
//! this captures a bounded, on-demand window that you drive by hand — walk into
//! the scene you want to measure, tap F10, do the thing, tap F10 again.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fpsmaster_render::RenderStats;

/// One rendered frame's measurements.
struct Sample {
    /// Wall-clock interval since the previously sampled frame, microseconds.
    frame_us: u32,
    stats: RenderStats,
    /// This frame's per-phase CPU cost (microseconds), from the frame profiler.
    phases: BTreeMap<&'static str, u32>,
}

/// A live capture session. Created when F10 starts a capture, consumed by
/// [`PerfCapture::finish`] when F10 stops it.
pub struct PerfCapture {
    start: Instant,
    last: Instant,
    samples: Vec<Sample>,
}

impl PerfCapture {
    pub fn new(now: Instant) -> Self {
        Self { start: now, last: now, samples: Vec::new() }
    }

    /// Seconds elapsed since the capture started (for the on-screen indicator).
    pub fn elapsed(&self, now: Instant) -> f32 {
        (now - self.start).as_secs_f32()
    }

    /// Frames sampled so far (for the on-screen indicator).
    pub fn frames(&self) -> usize {
        self.samples.len()
    }

    /// Record the just-rendered frame. `stats` is `renderer.last_stats()` and
    /// `phases` is `profiler.last_frame_phases()` — the CPU phase durations of
    /// the frame that just closed.
    pub fn record(
        &mut self,
        stats: RenderStats,
        phases: &BTreeMap<&'static str, Duration>,
        now: Instant,
    ) {
        // Cap the interval so a single hitch (alt-tab, a GC-style stall) doesn't
        // blow the totals; 100 ms == 10 fps is already deep into "spike".
        let frame_us = (now - self.last).as_micros().min(100_000) as u32;
        self.last = now;
        let phases = phases
            .iter()
            .map(|(k, v)| (*k, v.as_micros() as u32))
            .collect();
        self.samples.push(Sample { frame_us, stats, phases });
    }

    /// Close the capture: log a summary and write the per-frame CSV. Returns a
    /// short human-readable status line for the caller to surface (log/toast),
    /// e.g. `"perf: 512 frames, 143 fps avg -> perf_capture_1720000000.csv"`.
    pub fn finish(self, now: Instant) -> String {
        let n = self.samples.len();
        let secs = (now - self.start).as_secs_f32();
        if n == 0 {
            log::info!("[perf] capture stopped with no frames sampled");
            return "perf: no frames captured".to_owned();
        }

        // Frame-time percentiles (sorted copy) + the 1%-low fps (average of the
        // slowest 1% of frames, the number that "feels" like stutter).
        let mut ft: Vec<f32> =
            self.samples.iter().map(|s| s.frame_us as f32 / 1000.0).collect();
        ft.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |q: f32| ft[((ft.len() as f32 * q) as usize).min(ft.len() - 1)];
        let (min_ms, max_ms) = (ft[0], ft[ft.len() - 1]);
        let avg_ms = ft.iter().sum::<f32>() / n as f32;
        let avg_fps = if secs > 0.0 { n as f32 / secs } else { 0.0 };
        let low1_n = (n / 100).max(1);
        let low1_ms = ft[n - low1_n..].iter().sum::<f32>() / low1_n as f32;
        let low1_fps = if low1_ms > 0.0 { 1000.0 / low1_ms } else { 0.0 };

        // Per-field averages over the whole capture.
        let mut sum = Accum::default();
        for s in &self.samples {
            sum.add(&s.stats);
        }
        let a = |v: u64| v as f64 / n as f64;

        // CPU phase aggregation: avg & max ms per subsystem, ordered by avg desc.
        let names: BTreeSet<&'static str> =
            self.samples.iter().flat_map(|s| s.phases.keys().copied()).collect();
        let mut phase_stats: Vec<(&'static str, f64, f64)> = names
            .iter()
            .map(|&name| {
                let mut total = 0u64;
                let mut max = 0u32;
                for s in &self.samples {
                    let v = s.phases.get(name).copied().unwrap_or(0);
                    total += v as u64;
                    max = max.max(v);
                }
                (name, total as f64 / n as f64 / 1000.0, max as f64 / 1000.0)
            })
            .collect();
        phase_stats.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap());

        let mut report = String::new();
        let _ = writeln!(report, "===== perf capture: {n} frames over {secs:.1}s =====");
        let _ = writeln!(
            report,
            "  fps      avg {avg_fps:5.0}   1%-low {low1_fps:5.0}",
        );
        let _ = writeln!(
            report,
            "  frame ms min {min_ms:5.2}  avg {avg_ms:5.2}  p50 {:5.2}  p95 {:5.2}  p99 {:5.2}  max {max_ms:5.2}",
            pct(0.50),
            pct(0.95),
            pct(0.99),
        );
        let _ = writeln!(
            report,
            "  gpu ms   avg {:5.2}   (0 = adapter has no timestamp query)",
            a(sum.gpu_us) / 1000.0,
        );
        let _ = writeln!(
            report,
            "  cpu pass avg us  prepare {:5.0}  encode {:5.0}  submit {:5.0}  present {:5.0}  acquire {:5.0}",
            a(sum.prepare_us),
            a(sum.encode_us),
            a(sum.submit_us),
            a(sum.present_us),
            a(sum.acquire_us),
        );
        let _ = writeln!(
            report,
            "  draws    avg {:5.0}   visible chunks {:5.0}   tris {:.0}",
            a(sum.draw_calls),
            a(sum.visible_chunks),
            a(sum.chunk_indices) / 3.0,
        );
        report.push_str("  cpu phases (avg/max ms, worst first):\n");
        for (name, avg, max) in &phase_stats {
            if *max >= 0.01 {
                let _ = writeln!(report, "    {name:<12} {avg:6.3} / {max:6.3}");
            }
        }
        // Trim the trailing newline so the log block is tight.
        for line in report.trim_end().lines() {
            log::info!("{line}");
        }

        let csv_path = self.write_csv(&phase_stats);
        match &csv_path {
            Ok(path) => log::info!("[perf] per-frame data written to {path}"),
            Err(e) => log::warn!("[perf] could not write CSV: {e}"),
        }

        format!(
            "perf: {n} frames, {avg_fps:.0} fps avg, 1%-low {low1_fps:.0}{}",
            match csv_path {
                Ok(path) => format!(" -> {path}"),
                Err(_) => String::new(),
            }
        )
    }

    /// Write one row per sampled frame. Columns: the fixed render metrics, then
    /// one column per observed CPU phase (in the same order as the log summary).
    fn write_csv(
        &self,
        phase_stats: &[(&'static str, f64, f64)],
    ) -> std::io::Result<String> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = format!("perf_capture_{ts}.csv");

        let phase_cols: Vec<&'static str> = phase_stats.iter().map(|(n, _, _)| *n).collect();
        let mut out = String::new();
        out.push_str("frame,frame_ms,gpu_us,acquire_us,prepare_us,encode_us,submit_us,present_us,draw_calls,visible_chunks,tris");
        for name in &phase_cols {
            out.push(',');
            out.push_str(name);
            out.push_str("_us");
        }
        out.push('\n');

        for (i, s) in self.samples.iter().enumerate() {
            let st = &s.stats;
            let _ = write!(
                out,
                "{i},{:.3},{},{},{},{},{},{},{},{},{}",
                s.frame_us as f32 / 1000.0,
                st.gpu_us,
                st.acquire_us,
                st.prepare_us,
                st.encode_us,
                st.submit_us,
                st.present_us,
                st.draw_calls,
                st.visible_chunks,
                st.chunk_indices / 3,
            );
            for name in &phase_cols {
                let _ = write!(out, ",{}", s.phases.get(name).copied().unwrap_or(0));
            }
            out.push('\n');
        }

        std::fs::write(&path, out)?;
        // Report the absolute path when we can resolve it, else the relative one.
        Ok(std::fs::canonicalize(&path)
            .ok()
            .and_then(|p| p.to_str().map(str::to_owned))
            .unwrap_or(path))
    }
}

/// Running u64 sums of the `RenderStats` fields, to sidestep u32 overflow over a
/// long capture (draw counts × thousands of frames).
#[derive(Default)]
struct Accum {
    gpu_us: u64,
    acquire_us: u64,
    prepare_us: u64,
    encode_us: u64,
    submit_us: u64,
    present_us: u64,
    draw_calls: u64,
    visible_chunks: u64,
    chunk_indices: u64,
}

impl Accum {
    fn add(&mut self, s: &RenderStats) {
        self.gpu_us += s.gpu_us as u64;
        self.acquire_us += s.acquire_us as u64;
        self.prepare_us += s.prepare_us as u64;
        self.encode_us += s.encode_us as u64;
        self.submit_us += s.submit_us as u64;
        self.present_us += s.present_us as u64;
        self.draw_calls += s.draw_calls as u64;
        self.visible_chunks += s.visible_chunks as u64;
        self.chunk_indices += s.chunk_indices as u64;
    }
}
