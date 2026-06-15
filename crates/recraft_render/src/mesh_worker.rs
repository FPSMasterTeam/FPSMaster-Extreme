use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{channel, Receiver, Sender},
    Arc, Mutex,
};
use std::thread;

use recraft_core::SectionPos;

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
}

impl MeshWorker {
    pub fn new(atlas: AtlasUv, biome: BiomeColors) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (result_tx, result_rx) = channel::<(SectionPos, u64, ChunkMesh)>();
        // The atlas is read-only and shared by every worker.
        let atlas = Arc::new(atlas);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let fast_leaves = Arc::new(AtomicBool::new(false));

        let threads = thread::available_parallelism()
            .map(|n| n.get().saturating_sub(2))
            .unwrap_or(2)
            .clamp(1, 4);
        for index in 0..threads {
            let job_rx = Arc::clone(&job_rx);
            let result_tx = result_tx.clone();
            let atlas = Arc::clone(&atlas);
            let fast_leaves = Arc::clone(&fast_leaves);
            let _ = thread::Builder::new()
                .name(format!("mesh-worker-{index}"))
                .spawn(move || {
                    loop {
                        // Hold the receiver lock only while dequeuing; the mesh
                        // build below runs lock-free so workers run in parallel.
                        let job = match job_rx.lock().unwrap().recv() {
                            Ok(job) => job,
                            Err(_) => break, // sender dropped — shut down.
                        };
                        let col = job.neighborhood.position();
                        let pos = SectionPos::new(col.x, job.section_y, col.z);
                        let mesh = build_section_mesh_neighborhood(
                            &job.neighborhood,
                            job.section_y,
                            &atlas,
                            biome,
                            fast_leaves.load(Ordering::Relaxed),
                        );
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
        }
    }

    /// Set the Fast-leaves flag future mesh jobs read. The caller must re-queue
    /// the loaded sections for the change to reach already-built meshes.
    pub fn set_fast_leaves(&self, on: bool) {
        self.fast_leaves.store(on, Ordering::Relaxed);
    }

    /// Queue one section of a column snapshot for meshing.
    pub fn submit(&self, neighborhood: Arc<ChunkNeighborhood>, section_y: i32, generation: u64) {
        let _ = self.job_tx.send(Job {
            neighborhood,
            section_y,
            generation,
        });
    }

    /// Take one finished mesh, if any is ready.
    pub fn try_recv(&self) -> Option<(SectionPos, u64, ChunkMesh)> {
        self.result_rx.try_recv().ok()
    }
}
