//! PERF-03: Eight-fragment AVX2/FMA SDF shading, sharing the scalar atlas
//! addressing, tile ordering and final quantization contracts.

use std::arch::x86_64::*;

use super::super::PlannedCommand;
use super::*;

/// The caller checks AVX2/FMA and supplies eight contiguous tile pixels.
#[target_feature(enable = "avx2,fma")]
pub(super) unsafe fn shade_blend_packet(
    atlas: &impl SdfAtlasSource,
    command: &PlannedCommand,
    x: u32,
    y: u32,
    destination: &mut [[f32; 4]],
) -> Result<(), SdfTileError> {
    let offsets = _mm256_setr_ps(0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0);
    let px = _mm256_add_ps(_mm256_set1_ps(x as f32 + PIXEL_CENTRE), offsets);
    let py = _mm256_set1_ps(y as f32 + PIXEL_CENTRE);
    let tx = _mm256_fmadd_ps(
        _mm256_set1_ps(command.tx_dx),
        px,
        _mm256_fmadd_ps(
            _mm256_set1_ps(command.tx_dy),
            py,
            _mm256_set1_ps(command.tx_c),
        ),
    );
    let ty = _mm256_fmadd_ps(
        _mm256_set1_ps(command.ty_dx),
        px,
        _mm256_fmadd_ps(
            _mm256_set1_ps(command.ty_dy),
            py,
            _mm256_set1_ps(command.ty_c),
        ),
    );
    let mut xs = [0.0f32; 8];
    let mut ys = [0.0f32; 8];
    unsafe {
        _mm256_storeu_ps(xs.as_mut_ptr(), tx);
        _mm256_storeu_ps(ys.as_mut_ptr(), ty);
    }
    // The checked sampler also supports runtime EDT pages and atlas-rectangle
    // edge clamps; no unmasked gather can read beyond a mapped page.
    let mut samples = [sample_bilinear(
        atlas,
        command.source.atlas_set,
        command.source.atlas_page,
        command.source.atlas_rect,
        xs[0],
        ys[0],
    )?; 8];
    for lane in 1..8 {
        samples[lane] = sample_bilinear(
            atlas,
            command.source.atlas_set,
            command.source.atlas_page,
            command.source.atlas_rect,
            xs[lane],
            ys[lane],
        )?;
    }
    let mut source_channels = [[0.0f32; 8]; 4];
    if let (SdfPrimitiveKind::Text, SdfCommandMaterial::Text(material)) =
        (command.source.kind, command.source.material)
    {
        let mut values = [[0.0f32; 8]; 4];
        let mut fx = [0.0f32; 8];
        let mut fy = [0.0f32; 8];
        for (lane, sample) in samples.iter().enumerate() {
            fx[lane] = sample.fx;
            fy[lane] = sample.fy;
            for (index, texel) in sample.texels.iter().enumerate() {
                values[index][lane] = match texel {
                    SdfAtlasTexel::Text(value) => f32::from(*value) / 255.0,
                    _ => {
                        return Err(SdfTileError::AtlasFormatMismatch {
                            kind: command.source.kind,
                        })
                    }
                };
            }
        }
        let p00 = unsafe { _mm256_loadu_ps(values[0].as_ptr()) };
        let p10 = unsafe { _mm256_loadu_ps(values[1].as_ptr()) };
        let p01 = unsafe { _mm256_loadu_ps(values[2].as_ptr()) };
        let p11 = unsafe { _mm256_loadu_ps(values[3].as_ptr()) };
        let fx = unsafe { _mm256_loadu_ps(fx.as_ptr()) };
        let fy = unsafe { _mm256_loadu_ps(fy.as_ptr()) };
        let top = _mm256_fmadd_ps(_mm256_sub_ps(p10, p00), fx, p00);
        let bottom = _mm256_fmadd_ps(_mm256_sub_ps(p11, p01), fx, p01);
        let sdf = _mm256_fmadd_ps(_mm256_sub_ps(bottom, top), fy, top);
        let face = _mm256_min_ps(
            _mm256_set1_ps(1.0),
            _mm256_max_ps(
                _mm256_setzero_ps(),
                _mm256_fmadd_ps(
                    _mm256_set1_ps(material.face_scale.max(TMP_MIN_SHADER_SCALE)),
                    sdf,
                    _mm256_set1_ps(-material.face_bias),
                ),
            ),
        );
        let outline = _mm256_min_ps(
            _mm256_set1_ps(1.0),
            _mm256_max_ps(
                _mm256_setzero_ps(),
                _mm256_fmadd_ps(
                    _mm256_set1_ps(material.outline_scale.max(TMP_MIN_SHADER_SCALE)),
                    sdf,
                    _mm256_set1_ps(-material.outline_bias),
                ),
            ),
        );
        // Keep the scalar oracle's separate multiply/subtract here. Fusing
        // this expression would change rounding at material boundaries.
        let weight = _mm256_mul_ps(
            outline,
            _mm256_sub_ps(
                _mm256_set1_ps(1.0),
                _mm256_mul_ps(_mm256_set1_ps(material.face[3]), face),
            ),
        );
        for (channel, output) in source_channels.iter_mut().enumerate() {
            let color = _mm256_mul_ps(
                _mm256_fmadd_ps(
                    _mm256_set1_ps(material.outline[channel]),
                    weight,
                    _mm256_mul_ps(_mm256_set1_ps(material.face[channel]), face),
                ),
                _mm256_set1_ps(material.vertex_alpha.clamp(0.0, 1.0)),
            );
            unsafe {
                _mm256_storeu_ps(output.as_mut_ptr(), color);
            }
        }
    } else {
        for (lane, sample) in samples.into_iter().enumerate() {
            let source = shade_sample(command.source, sample)?;
            for channel in 0..4 {
                source_channels[channel][lane] = source[channel];
            }
        }
    }
    let inverse_alpha = _mm256_sub_ps(_mm256_set1_ps(1.0), unsafe {
        _mm256_loadu_ps(source_channels[3].as_ptr())
    });
    for (channel, source) in source_channels.iter().enumerate() {
        let mut values = std::array::from_fn::<_, 8, _>(|lane| destination[lane][channel]);
        let blended = _mm256_fmadd_ps(
            unsafe { _mm256_loadu_ps(values.as_ptr()) },
            inverse_alpha,
            unsafe { _mm256_loadu_ps(source.as_ptr()) },
        );
        unsafe {
            _mm256_storeu_ps(values.as_mut_ptr(), blended);
        }
        for lane in 0..8 {
            destination[lane][channel] = values[lane];
        }
    }
    Ok(())
}
