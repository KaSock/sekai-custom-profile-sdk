//! PERF-03: AVX2 image-coordinate packets and byte-exact RGBA8 blending.

use sekai_profile_core::{BlendMode, Matrix2d};
use std::arch::x86_64::*;

#[target_feature(enable = "avx2,fma")]
pub(super) unsafe fn local_coordinates(x: u32, y: u32, inverse: &Matrix2d) -> ([f32; 8], [f32; 8]) {
    let px = _mm256_add_ps(
        _mm256_set1_ps(x as f32 + 0.5),
        _mm256_setr_ps(0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0),
    );
    let py = _mm256_set1_ps(y as f32 + 0.5);
    let mut xs = [0.0f32; 8];
    let mut ys = [0.0f32; 8];
    let local_x = _mm256_fmadd_ps(
        _mm256_set1_ps(inverse[0]),
        px,
        _mm256_fmadd_ps(_mm256_set1_ps(inverse[2]), py, _mm256_set1_ps(inverse[4])),
    );
    let local_y = _mm256_fmadd_ps(
        _mm256_set1_ps(inverse[1]),
        px,
        _mm256_fmadd_ps(_mm256_set1_ps(inverse[3]), py, _mm256_set1_ps(inverse[5])),
    );
    unsafe {
        _mm256_storeu_ps(xs.as_mut_ptr(), local_x);
        _mm256_storeu_ps(ys.as_mut_ptr(), local_y);
    }
    (xs, ys)
}

#[target_feature(enable = "avx2")]
fn mul_div_255(a: __m256i, b: __m256i) -> __m256i {
    let product = _mm256_add_epi16(_mm256_mullo_epi16(a, b), _mm256_set1_epi16(128));
    _mm256_srli_epi16::<8>(_mm256_add_epi16(product, _mm256_srli_epi16::<8>(product)))
}

#[target_feature(enable = "avx2")]
fn blend_channels(
    source: __m256i,
    dest: __m256i,
    sa: __m256i,
    da: __m256i,
    mode: BlendMode,
) -> __m256i {
    let max = _mm256_set1_epi16(255);
    let inv_sa = _mm256_sub_epi16(max, sa);
    match mode {
        BlendMode::SrcOver => _mm256_add_epi16(source, mul_div_255(dest, inv_sa)),
        BlendMode::SrcIn => mul_div_255(source, da),
        BlendMode::DstIn => mul_div_255(dest, sa),
        BlendMode::Multiply => _mm256_add_epi16(
            mul_div_255(source, dest),
            _mm256_add_epi16(
                mul_div_255(source, _mm256_sub_epi16(max, da)),
                mul_div_255(dest, inv_sa),
            ),
        ),
        BlendMode::Screen => {
            _mm256_sub_epi16(_mm256_add_epi16(source, dest), mul_div_255(source, dest))
        }
        BlendMode::Add => _mm256_add_epi16(source, dest),
    }
}

/// Only active pixels are read/written. The caller checks AVX2 and provides
/// up to sixteen valid source/destination pixels selected by the mask.
#[target_feature(enable = "avx2")]
pub(super) unsafe fn blend_rgba8_packet(
    destination: *mut u8,
    source: *const u32,
    active: u16,
    mode: BlendMode,
) {
    for group in 0..2 {
        let bits = (active >> (group * 8)) as u8;
        if bits == 0 {
            continue;
        }
        let mask_lanes: [i32; 8] =
            std::array::from_fn(|lane| if bits & (1 << lane) != 0 { -1 } else { 0 });
        let mask = unsafe { _mm256_loadu_si256(mask_lanes.as_ptr().cast()) };
        let src = unsafe { _mm256_maskload_epi32(source.add(group * 8).cast(), mask) };
        let dest_ptr = unsafe { destination.add(group * 32).cast::<i32>() };
        let dst = unsafe { _mm256_maskload_epi32(dest_ptr, mask) };
        let sa = _mm256_mullo_epi32(_mm256_srli_epi32::<24>(src), _mm256_set1_epi32(0x01010101));
        let da = _mm256_mullo_epi32(_mm256_srli_epi32::<24>(dst), _mm256_set1_epi32(0x01010101));
        let zero = _mm256_setzero_si256();
        let low = blend_channels(
            _mm256_unpacklo_epi8(src, zero),
            _mm256_unpacklo_epi8(dst, zero),
            _mm256_unpacklo_epi8(sa, zero),
            _mm256_unpacklo_epi8(da, zero),
            mode,
        );
        let high = blend_channels(
            _mm256_unpackhi_epi8(src, zero),
            _mm256_unpackhi_epi8(dst, zero),
            _mm256_unpackhi_epi8(sa, zero),
            _mm256_unpackhi_epi8(da, zero),
            mode,
        );
        unsafe {
            _mm256_maskstore_epi32(dest_ptr, mask, _mm256_packus_epi16(low, high));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_byte_products_match_scalar_exhaustively() {
        // The integer identity used by the vector kernel has no half-way ties.
        for a in 0u32..=255 {
            for b in 0u32..=255 {
                let product = a * b + 128;
                assert_eq!((product + (product >> 8)) >> 8, (a * b + 127) / 255);
            }
        }
    }

    #[test]
    fn masked_blends_match_scalar_for_every_mode_and_tail() {
        if !crate::sdf_avx2_available() {
            return;
        }
        for mode in [
            BlendMode::SrcOver,
            BlendMode::SrcIn,
            BlendMode::DstIn,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Add,
        ] {
            for count in 1..=16 {
                for seed in 0..256u32 {
                    let source: Vec<u32> = (0..count)
                        .map(|lane| {
                            let a = (seed + lane as u32 * 17) as u8;
                            u32::from_le_bytes([a / 2, a / 3, a, a])
                        })
                        .collect();
                    // The prefix makes the pointer unaligned. No padding follows
                    // the final pixel, so masked tails must not touch later lanes.
                    let mut actual = vec![91; 1 + count * 4];
                    for (lane, pixel) in actual[1..].chunks_exact_mut(4).enumerate() {
                        let a = (255 - seed + lane as u32 * 13) as u8;
                        pixel.copy_from_slice(&[a, a / 2, a / 5, a]);
                    }
                    let mut expected = actual.clone();
                    let full = if count == 16 {
                        u16::MAX
                    } else {
                        (1u16 << count) - 1
                    };
                    let active = full & if seed % 2 == 0 { u16::MAX } else { 0xa55a };
                    for lane in 0..count {
                        if active & (1 << lane) != 0 {
                            super::super::blend_pixel(
                                &mut expected[1 + lane * 4..1 + (lane + 1) * 4],
                                source[lane].to_le_bytes(),
                                mode,
                            );
                        }
                    }
                    // SAFETY: features checked above; active bits select valid
                    // pixels in the minimally sized source and destination.
                    unsafe {
                        blend_rgba8_packet(
                            actual.as_mut_ptr().add(1),
                            source.as_ptr(),
                            active,
                            mode,
                        );
                    }
                    assert_eq!(actual, expected, "{mode:?}, count={count}, seed={seed}");
                }
            }
        }
    }
}
