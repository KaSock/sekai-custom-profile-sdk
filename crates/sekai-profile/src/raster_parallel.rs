//! PERF-ANIM-03: One CPU pool for native SDF rasterization and animation fills/blits.
//! Keeps SEKAI_PROFILE_RASTER_THREADS at 1..=64, default available CPUs capped at 8.

pub(crate) fn raster_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::env::var("SEKAI_PROFILE_RASTER_THREADS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|threads| (1..=64).contains(threads))
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(1)
                    .min(8)
            });
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("profile-raster-{index}"))
            .build()
            .expect("profile raster thread pool creation failed")
    })
}
