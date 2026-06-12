use std::sync::{
    mpsc::{channel, Receiver, Sender},
    Arc, Mutex,
};
use std::thread;

use recraft_core::ChunkPos;

use crate::chunk_mesh::{build_chunk_mesh_neighborhood, BiomeColors, ChunkMesh, ChunkNeighborhood};
use crate::AtlasUv;

struct Job {
    neighborhood: ChunkNeighborhood,
}

/// A small pool of worker threads that turn chunk snapshots into CPU mesh
/// buffers off the render thread. Submitting is cheap (just a snapshot clone);
/// the expensive meshing runs in the background and finished meshes are picked
/// up by the render loop, so chunk (re)meshing never stalls a frame.
pub struct MeshWorker {
    job_tx: Sender<Job>,
    result_rx: Receiver<(ChunkPos, ChunkMesh)>,
}

impl MeshWorker {
    pub fn new(atlas: AtlasUv, biome: BiomeColors) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (result_tx, result_rx) = channel::<(ChunkPos, ChunkMesh)>();
        // The atlas is read-only and shared by every worker.
        let atlas = Arc::new(atlas);
        let job_rx = Arc::new(Mutex::new(job_rx));

        let threads = thread::available_parallelism()
            .map(|n| n.get().saturating_sub(2))
            .unwrap_or(2)
            .clamp(1, 4);
        for index in 0..threads {
            let job_rx = Arc::clone(&job_rx);
            let result_tx = result_tx.clone();
            let atlas = Arc::clone(&atlas);
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
                        let pos = job.neighborhood.position();
                        let mesh = build_chunk_mesh_neighborhood(&job.neighborhood, &atlas, biome);
                        if result_tx.send((pos, mesh)).is_err() {
                            break;
                        }
                    }
                });
        }

        Self { job_tx, result_rx }
    }

    /// Queue a chunk snapshot for meshing.
    pub fn submit(&self, neighborhood: ChunkNeighborhood) {
        let _ = self.job_tx.send(Job { neighborhood });
    }

    /// Take one finished mesh, if any is ready.
    pub fn try_recv(&self) -> Option<(ChunkPos, ChunkMesh)> {
        self.result_rx.try_recv().ok()
    }
}
