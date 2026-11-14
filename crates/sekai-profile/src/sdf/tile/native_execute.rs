//! Native page and animation execution over disjoint, tile-aligned row bands.

use super::{SdfAtlasSource, SdfDestination, SdfExecutionStats, SdfTileError, SdfTilePlan};
use crate::render_config::Executor;

// PERF-ANIM-03: SDF row bands and animation frame rows share one bounded pool.
#[cfg(feature = "parallel")]
use crate::raster_parallel::raster_pool;

impl SdfTilePlan {
    pub(crate) fn execute_native_f32(
        &self,
        atlas: &(impl SdfAtlasSource + Sync),
        executor: Executor,
        destination: SdfDestination,
        output: &mut [u8],
    ) -> Result<SdfExecutionStats, SdfTileError> {
        #[cfg(not(feature = "parallel"))]
        {
            match (executor, destination) {
                (Executor::Avx2, SdfDestination::Clear(clear)) => {
                    self.execute_avx2_f32(atlas, clear, output)
                }
                (Executor::Avx2, SdfDestination::LoadExisting) => {
                    self.execute_avx2_f32_over(atlas, output)
                }
                (Executor::Scalar, SdfDestination::Clear(clear)) => {
                    self.execute_scalar_f32(atlas, clear, output)
                }
                (Executor::Scalar, SdfDestination::LoadExisting) => {
                    self.execute_scalar_f32_over(atlas, output)
                }
                (Executor::Simd, SdfDestination::Clear(clear)) => {
                    self.execute_simd(atlas, clear, output, super::SdfAccumulationMode::F32Tile)
                }
                (Executor::Simd, SdfDestination::LoadExisting) => {
                    self.execute_simd_f32_over(atlas, output)
                }
            }
        }
        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;

            let row_bytes = (self.grid.canvas_width as usize)
                .checked_mul(4)
                .ok_or(SdfTileError::SizeOverflow)?;
            let expected = row_bytes
                .checked_mul(self.grid.canvas_height as usize)
                .ok_or(SdfTileError::SizeOverflow)?;
            if output.len() != expected {
                return Err(SdfTileError::OutputLength {
                    expected,
                    actual: output.len(),
                });
            }
            if executor == Executor::Simd && !crate::render_config::sdf_simd_available() {
                return Err(SdfTileError::SimdUnavailable);
            }
            let band_bytes = row_bytes
                .checked_mul(usize::from(self.grid.tile_height))
                .ok_or(SdfTileError::SizeOverflow)?;
            raster_pool().install(|| {
                output
                    .par_chunks_mut(band_bytes)
                    .enumerate()
                    .map(|(index, band)| {
                        let first_y = index as u32 * u32::from(self.grid.tile_height);
                        match executor {
                            Executor::Avx2 => {
                                self.execute_avx2_rows(atlas, destination, band, first_y)
                            }
                            Executor::Scalar => self.execute_scalar_rows(
                                atlas,
                                destination,
                                band,
                                super::SdfAccumulationMode::F32Tile,
                                false,
                                first_y,
                            ),
                            Executor::Simd => {
                                #[cfg(target_arch = "x86_64")]
                                // SAFETY: CPU features were checked above. Each band is
                                // disjoint, starts on a tile boundary and ends on a
                                // tile boundary or the bottom of the validated canvas.
                                unsafe {
                                    super::simd_x86::execute_rows(
                                        self,
                                        atlas,
                                        destination,
                                        band,
                                        super::SdfAccumulationMode::F32Tile,
                                        first_y,
                                    )
                                }
                                #[cfg(not(target_arch = "x86_64"))]
                                Err(SdfTileError::SimdUnavailable)
                            }
                        }
                    })
                    .try_reduce(SdfExecutionStats::default, |mut total, value| {
                        total.accumulate(value);
                        Ok(total)
                    })
            })
        }
    }
}
