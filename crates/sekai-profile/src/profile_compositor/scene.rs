//! Scene entry points: whole-scene image renders, authored elements composited
//! into an existing surface, and the scene's visible device bounds.

use std::collections::BTreeSet;

use sekai_profile_core::profile_scene::ResolvedProfileScene;
use sekai_profile_core::{AuthoredElementKind, BlendMode, SemanticCommandPayload, StableId};

use super::{
    canvas_bytes, collect_layers, compose_matrix, evaluate_command_control_state,
    render_image_commands_indexed, render_image_commands_into, transform_point,
    try_render_general_base_into, unsupported, CommandFilter, CompositorSources,
    GeneralBaseAttempt, ImageExecutor, ProfileCompositorError, ProfileCompositorOutput,
    ProfileCompositorStats, ProfileSceneIndex, RgbaSurface, PROFILE_COMPOSITOR_SCHEMA,
};
use crate::render_object::MappedRenderObjectStore;

/// Render only Image and Composite commands. Text and Shape are intentionally
/// skipped so this path can be quantified before it is joined to the SDF tile
/// executor; unsupported image semantics return an error instead of drifting.
pub fn render_image_scene_scalar(
    scene: &ResolvedProfileScene,
    store: &MappedRenderObjectStore,
    width: u32,
    height: u32,
) -> Result<ProfileCompositorOutput, ProfileCompositorError> {
    render_image_scene(scene, store, width, height, ImageExecutor::Scalar)
}

/// Render the same Image/Composite subset with the AVX-512 packet executor.
/// The scalar entry point is the byte reference it must match.
pub fn render_image_scene_simd(
    scene: &ResolvedProfileScene,
    store: &MappedRenderObjectStore,
    width: u32,
    height: u32,
) -> Result<ProfileCompositorOutput, ProfileCompositorError> {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx512f")
        && std::arch::is_x86_feature_detected!("avx512bw")
    {
        return render_image_scene(scene, store, width, height, ImageExecutor::Simd);
    }
    unsupported("profile-compositor", "AVX-512F+AVX-512BW packet executor")
}

fn render_image_scene(
    scene: &ResolvedProfileScene,
    store: &MappedRenderObjectStore,
    width: u32,
    height: u32,
    executor: ImageExecutor,
) -> Result<ProfileCompositorOutput, ProfileCompositorError> {
    let pixel_bytes = canvas_bytes(width, height)?;
    let mut pixels = vec![0; pixel_bytes];
    let stats = render_image_commands_into(
        scene,
        CompositorSources::new(store),
        CommandFilter::default(),
        RgbaSurface::new(&mut pixels, width, height)?,
        executor,
    )?;
    Ok(ProfileCompositorOutput {
        schema: PROFILE_COMPOSITOR_SCHEMA.into(),
        width,
        height,
        pixels,
        stats,
    })
}

/// Composites the Image and Composite commands of one authored element into
/// `surface` with the packet executor.
pub fn render_authored_image_into_simd(
    scene: &ResolvedProfileScene,
    store: &MappedRenderObjectStore,
    authored_kind: AuthoredElementKind,
    authored_index: u32,
    surface: RgbaSurface<'_>,
) -> Result<ProfileCompositorStats, ProfileCompositorError> {
    #[cfg(target_arch = "x86_64")]
    if !(std::arch::is_x86_feature_detected!("avx512f")
        && std::arch::is_x86_feature_detected!("avx512bw"))
    {
        return unsupported("profile-compositor", "AVX-512F+AVX-512BW packet executor");
    }
    #[cfg(not(target_arch = "x86_64"))]
    return unsupported("profile-compositor", "x86_64 packet executor");

    #[allow(unreachable_code)]
    let layer_ids = authored_layer_ids(scene, authored_kind, authored_index)?;
    render_image_commands_into(
        scene,
        CompositorSources::new(store),
        CommandFilter {
            layers: Some(&layer_ids),
            commands: None,
        },
        surface,
        ImageExecutor::Simd,
    )
}

/// Composites every command of one authored element into `surface` with the
/// packet executor, from its baked General base when one applies.
pub fn render_authored_profile_into_simd(
    scene: &ResolvedProfileScene,
    sources: CompositorSources<'_>,
    authored_kind: AuthoredElementKind,
    authored_index: u32,
    surface: RgbaSurface<'_>,
) -> Result<ProfileCompositorStats, ProfileCompositorError> {
    #[cfg(target_arch = "x86_64")]
    if !(std::arch::is_x86_feature_detected!("avx512f")
        && std::arch::is_x86_feature_detected!("avx512bw"))
    {
        return unsupported("profile-compositor", "AVX-512F+AVX-512BW packet executor");
    }
    #[cfg(not(target_arch = "x86_64"))]
    return unsupported("profile-compositor", "x86_64 packet executor");

    #[allow(unreachable_code)]
    render_authored_profile_into(
        scene,
        sources,
        authored_kind,
        authored_index,
        surface,
        ImageExecutor::Simd,
        None,
    )
}

/// PERF-03: AVX2/FMA packet rendering of one authored element, with checked
/// source sampling and the scalar compositor's premultiplied pixel contract.
pub fn render_authored_profile_into_avx2(
    scene: &ResolvedProfileScene,
    sources: CompositorSources<'_>,
    authored_kind: AuthoredElementKind,
    authored_index: u32,
    surface: RgbaSurface<'_>,
) -> Result<ProfileCompositorStats, ProfileCompositorError> {
    if !crate::render_config::sdf_avx2_available() {
        return unsupported("profile-compositor", "AVX2+FMA packet executor");
    }
    render_authored_profile_into(
        scene,
        sources,
        authored_kind,
        authored_index,
        surface,
        ImageExecutor::Avx2,
        None,
    )
}

/// Scalar-executor twin of [`render_authored_profile_into_simd`], for hosts
/// without the packet instruction set. Byte-equal output; slower.
/// Scalar entry point with the same pixel contract as both packet executors.
pub fn render_authored_profile_into_scalar(
    scene: &ResolvedProfileScene,
    sources: CompositorSources<'_>,
    authored_kind: AuthoredElementKind,
    authored_index: u32,
    surface: RgbaSurface<'_>,
) -> Result<ProfileCompositorStats, ProfileCompositorError> {
    render_authored_profile_into(
        scene,
        sources,
        authored_kind,
        authored_index,
        surface,
        ImageExecutor::Scalar,
        None,
    )
}

/// The layers an authored element lowered to; an element with none is an
/// error.
fn authored_layer_ids(
    scene: &ResolvedProfileScene,
    authored_kind: AuthoredElementKind,
    authored_index: u32,
) -> Result<BTreeSet<StableId>, ProfileCompositorError> {
    let layer_ids = scene
        .layers
        .iter()
        .filter(|layer| {
            layer.authored_kind == authored_kind && layer.authored_index == authored_index
        })
        .map(|layer| layer.id)
        .collect::<BTreeSet<_>>();
    if layer_ids.is_empty() {
        return Err(ProfileCompositorError::MissingLayer(format!(
            "{authored_kind:?}:{authored_index}"
        )));
    }
    Ok(layer_ids)
}

/// Composites the commands of one authored element into `surface`, from its
/// baked General base when one applies.
#[allow(clippy::too_many_arguments)]
fn render_authored_profile_into(
    scene: &ResolvedProfileScene,
    sources: CompositorSources<'_>,
    authored_kind: AuthoredElementKind,
    authored_index: u32,
    mut surface: RgbaSurface<'_>,
    executor: ImageExecutor,
    index: Option<&ProfileSceneIndex<'_>>,
) -> Result<ProfileCompositorStats, ProfileCompositorError> {
    let indexed = index.and_then(|index| index.authored.get(&(authored_kind, authored_index)));
    let owned_layer_ids;
    let layer_ids = if let Some((layers, _)) = indexed {
        layers
    } else {
        owned_layer_ids = authored_layer_ids(scene, authored_kind, authored_index)?;
        &owned_layer_ids
    };
    let mut general_base_miss = false;
    if authored_kind == AuthoredElementKind::General {
        match try_render_general_base_into(
            scene,
            sources,
            authored_index,
            surface.reborrow(),
            executor,
        )? {
            GeneralBaseAttempt::Hit(stats) => return Ok(*stats),
            GeneralBaseAttempt::Miss => general_base_miss = true,
            GeneralBaseAttempt::NotEligible => {}
        }
    }
    let mut stats = render_image_commands_indexed(
        scene,
        sources,
        CommandFilter {
            layers: Some(layer_ids),
            commands: None,
        },
        surface,
        executor,
        index,
        indexed.map(|(_, commands)| commands.as_slice()),
    )?;
    stats.general_base_miss_count = u64::from(general_base_miss);
    Ok(stats)
}

pub(crate) fn render_authored_profile_indexed(
    scene: &ResolvedProfileScene,
    sources: CompositorSources<'_>,
    authored: (AuthoredElementKind, u32),
    surface: RgbaSurface<'_>,
    executor: crate::render_config::Executor,
    index: Option<&ProfileSceneIndex<'_>>,
) -> Result<ProfileCompositorStats, ProfileCompositorError> {
    let executor = match executor {
        crate::render_config::Executor::Scalar => ImageExecutor::Scalar,
        crate::render_config::Executor::Avx2 if crate::render_config::sdf_avx2_available() => {
            ImageExecutor::Avx2
        }
        crate::render_config::Executor::Simd if crate::render_config::sdf_simd_available() => {
            ImageExecutor::Simd
        }
        _ => return unsupported("profile-compositor", "requested CPU executor"),
    };
    render_authored_profile_into(
        scene, sources, authored.0, authored.1, surface, executor, index,
    )
}

/// Returns true only when an authored element is a non-empty sequence of
/// SrcOver image commands. Rendering that sequence over transparent RGBA and
/// selecting its alpha-255 pixels then proves those destination pixels are
/// independent of everything below the element.
pub fn authored_image_is_exact_opaque_mask_source(
    scene: &ResolvedProfileScene,
    authored_kind: AuthoredElementKind,
    authored_index: u32,
) -> bool {
    let layer_ids = scene
        .layers
        .iter()
        .filter(|layer| {
            layer.authored_visible
                && layer.authored_kind == authored_kind
                && layer.authored_index == authored_index
        })
        .map(|layer| layer.id)
        .collect::<BTreeSet<_>>();
    if layer_ids.is_empty() {
        return false;
    }
    let mut image_count = 0usize;
    for command in scene
        .commands
        .iter()
        .filter(|command| layer_ids.contains(&command.layer_id))
    {
        match &command.payload {
            SemanticCommandPayload::Image { .. } if command.blend_mode == BlendMode::SrcOver => {
                image_count += 1;
            }
            _ => return false,
        }
    }
    image_count != 0
}

/// Conservative device-space bounds for all currently visible commands.
///
/// The result deliberately does not intersect command clips: animation layers
/// need a safe source raster before their per-frame transform is applied, and
/// a small guard covers filter/SDF fringes outside authored command bounds.
pub fn visible_scene_device_bounds(
    scene: &ResolvedProfileScene,
    guard: f32,
) -> Result<Option<(i32, i32, u32, u32)>, ProfileCompositorError> {
    let layers = collect_layers(scene)?;
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for command in &scene.commands {
        let layer = layers.get(&command.layer_id).copied().ok_or_else(|| {
            ProfileCompositorError::MissingLayer(format!("{:?}", command.layer_id))
        })?;
        let control_state = evaluate_command_control_state(scene, command)?;
        if !layer.authored_visible || !control_state.visible {
            continue;
        }
        let bounds = command.bounds;
        if !bounds.x.is_finite()
            || !bounds.y.is_finite()
            || !bounds.width.is_finite()
            || !bounds.height.is_finite()
            || bounds.width <= 0.0
            || bounds.height <= 0.0
        {
            continue;
        }
        let mut command_matrix = command.matrix;
        command_matrix[5] += control_state.translate_y;
        let matrix = compose_matrix(layer.matrix, command_matrix);
        for (x, y) in [
            transform_point(matrix, bounds.x, bounds.y),
            transform_point(matrix, bounds.x + bounds.width, bounds.y),
            transform_point(matrix, bounds.x + bounds.width, bounds.y + bounds.height),
            transform_point(matrix, bounds.x, bounds.y + bounds.height),
        ] {
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }

    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return Ok(None);
    }
    let guard = guard.max(0.0);
    let left = (min_x - guard).floor();
    let top = (min_y - guard).floor();
    let right = (max_x + guard).ceil();
    let bottom = (max_y + guard).ceil();
    if left < i32::MIN as f32
        || top < i32::MIN as f32
        || right > i32::MAX as f32
        || bottom > i32::MAX as f32
        || right <= left
        || bottom <= top
    {
        return Err(ProfileCompositorError::InvalidCanvas {
            width: 0,
            height: 0,
        });
    }
    let left = left as i32;
    let top = top as i32;
    let width = u32::try_from(right as i64 - left as i64).map_err(|_| {
        ProfileCompositorError::InvalidCanvas {
            width: 0,
            height: 0,
        }
    })?;
    let height = u32::try_from(bottom as i64 - top as i64)
        .map_err(|_| ProfileCompositorError::InvalidCanvas { width, height: 0 })?;
    canvas_bytes(width, height)?;
    Ok(Some((left, top, width, height)))
}
