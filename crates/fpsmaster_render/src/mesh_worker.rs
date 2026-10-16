use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{channel, Receiver, Sender},
    Arc, Mutex,
};
use std::thread;

use fpsmaster_core::SectionPos;

use crate::chunk_mesh::{
    build_section_mesh_neighborhood, BiomeColors, ChunkMesh, ChunkNeighborhood,
};
use crate::AtlasUv;

struct Job {
    /// The whole column plus its horizontal neighbours, shared (via `Arc`) by
    /// every dirty section of the same column so submitting N sections of one
    /// column clones the snapshot only once.
    neighborhood: Arc<ChunkNeighborhood>,
    section_y: i32,
    generation: u64,
}

/// A small pool of worker threads that turn chunk snapshots into CPU mesh
/// buffers off the render thread. Submitting is cheap (just a snapshot clone);
/// the expensive meshing runs in the background and finished meshes are picked
/// up by the render loop, so chunk (re)meshing never stalls a frame.
pub struct MeshWorker {
    job_tx: Sender<Job>,
    result_rx: Receiver<(SectionPos, u64, ChunkMesh)>,
    /// Fast-graphics leaves: read by every worker per job, flipped by the render
    /// thread on a graphics-mode toggle (the toggle also re-dirties all chunks).
    fast_leaves: Arc<AtomicBool>,
    /// Flat (greedy, smooth-lighting-off) meshing: same per-job read + toggle.
    flat: Arc<AtomicBool>,
    /// Jobs submitted whose result has not been taken yet. Each queued job pins
    /// a ~1-2 MB column snapshot, so the caller throttles submissions on this
    /// (see `capacity`).
    in_flight: usize,
    /// Upper bound on `in_flight` before `capacity` reports zero.
    max_in_flight: usize,
    /// Mesh builds that panicked (skipped; the section renders empty).
    failures: Arc<AtomicUsize>,
}

/// Queued-plus-unconsumed jobs allowed per worker thread before submissions
/// pause: enough to keep every worker busy across a frame, small enough that
/// a burst can't pile up hundreds of MB of snapshots.
const IN_FLIGHT_PER_THREAD: usize = 16;

impl MeshWorker {
    pub fn new(atlas: AtlasUv, biome: BiomeColors) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (result_tx, result_rx) = channel::<(SectionPos, u64, ChunkMesh)>();
        // The atlas is read-only and shared by every worker.
        let atlas = Arc::new(atlas);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let fast_leaves = Arc::new(AtomicBool::new(false));
        let flat = Arc::new(AtomicBool::new(false));
        let failures = Arc::new(AtomicUsize::new(0));

        let threads = thread::available_parallelism()
            .map(|n| n.get().saturating_sub(2))
            .unwrap_or(2)
            .clamp(1, 4);
        for index in 0..threads {
            let job_rx = Arc::clone(&job_rx);
            let result_tx = result_tx.clone();
            let atlas = Arc::clone(&atlas);
            // `BiomeColors` is Arc-backed, so each worker gets a cheap handle to
            // the same resolved colour table.
            let biome = biome.clone();
            let fast_leaves = Arc::clone(&fast_leaves);
            let flat = Arc::clone(&flat);
            let failures = Arc::clone(&failures);
            let _ = thread::Builder::new()
                .name(format!("mesh-worker-{index}"))
                .spawn(move || {
                    loop {
                        // Hold the receiver lock only while dequeuing; the mesh
                        // build below runs lock-free so workers run in parallel.
                        // The lock only guards `recv`, so a poisoned mutex holds no
                        // broken state; don't let one dead worker take the rest down.
                        let job = match job_rx
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .recv()
                        {
                            Ok(job) => job,
                            Err(_) => break, // sender dropped — shut down.
                        };
                        let col = job.neighborhood.position();
                        let pos = SectionPos::new(col.x, job.section_y, col.z);
                        // A mesher bug on some odd block state must not kill the
                        // worker (the pool would drain and meshing stop for good):
                        // log it, count it for F3, and deliver an empty mesh so the
                        // job is still accounted for.
                        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            build_section_mesh_neighborhood(
                                &job.neighborhood,
                                job.section_y,
                                &atlas,
                                &biome,
                                fast_leaves.load(Ordering::Relaxed),
                                flat.load(Ordering::Relaxed),
                            )
                        }));
                        let mesh = built.unwrap_or_else(|_| {
                            failures.fetch_add(1, Ordering::Relaxed);
                            log::error!("mesh build panicked for section {pos:?}; left empty");
                            ChunkMesh::default()
                        });
                        if result_tx.send((pos, job.generation, mesh)).is_err() {
                            break;
                        }
                    }
                });
        }

        Self {
            job_tx,
            result_rx,
            fast_leaves,
            flat,
            in_flight: 0,
            max_in_flight: threads * IN_FLIGHT_PER_THREAD,
            failures,
        }
    }

    /// How many more sections may be submitted before the backlog cap.
    pub fn capacity(&self) -> usize {
        self.max_in_flight.saturating_sub(self.in_flight)
    }

    /// Jobs submitted but not yet taken back with `try_recv`.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Mesh builds that panicked since this pool started.
    pub fn failures(&self) -> usize {
        self.failures.load(Ordering::Relaxed)
    }

    /// Set the Fast-leaves flag future mesh jobs read. The caller must re-queue
    /// the loaded sections for the change to reach already-built meshes.
    pub fn set_fast_leaves(&self, on: bool) {
        self.fast_leaves.store(on, Ordering::Relaxed);
    }

    /// Set the flat (greedy) meshing flag future jobs read. Like `set_fast_leaves`,
    /// the caller must re-queue loaded sections to rebuild existing meshes.
    pub fn set_flat(&self, on: bool) {
        self.flat.store(on, Ordering::Relaxed);
    }

    /// Queue one section of a column snapshot for meshing.
    pub fn submit(&mut self, neighborhood: Arc<ChunkNeighborhood>, section_y: i32, generation: u64) {
        let job = Job {
            neighborhood,
            section_y,
            generation,
        };
        if self.job_tx.send(job).is_ok() {
            self.in_flight += 1;
        } else {
            // Only possible if every worker thread is gone.
            log::error!("mesh workers are gone; section {section_y} not queued");
        }
    }

    /// Take one finished mesh, if any is ready.
    pub fn try_recv(&mut self) -> Option<(SectionPos, u64, ChunkMesh)> {
        let result = self.result_rx.try_recv().ok()?;
        self.in_flight = self.in_flight.saturating_sub(1);
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fpsmaster_core::{BlockState, ChunkPos, World};

    /// Submissions count against the backlog cap until their result is taken.
    #[test]
    fn in_flight_tracks_submit_and_receive() {
        let atlas = crate::TextureAtlasImage::load_default().uv_table();
        let mut worker = MeshWorker::new(atlas, BiomeColors::default());
        let cap = worker.capacity();
        assert!(cap > 0);
        let mut world = World::new();
        world.set_block(0, 0, 0, BlockState::STONE);
        let snapshot = Arc::new(ChunkNeighborhood::snapshot(&world, ChunkPos::new(0, 0)).unwrap());
        for _ in 0..3 {
            worker.submit(Arc::clone(&snapshot), 0, 1);
        }
        assert_eq!(worker.capacity(), cap - 3);
        let mut received = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while received < 3 && std::time::Instant::now() < deadline {
            if worker.try_recv().is_some() {
                received += 1;
            } else {
                thread::yield_now();
            }
        }
        assert_eq!(received, 3);
        assert_eq!(worker.capacity(), cap);
        assert_eq!(worker.failures(), 0);
    }
}
