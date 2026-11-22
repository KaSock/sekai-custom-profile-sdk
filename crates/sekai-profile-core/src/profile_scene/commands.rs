//! Builders of the image, composite and text commands of component lowering.

use super::keys::semantic_command_id;
use super::layer::rect_quad;
use super::snapshot::ResourceDescriptor;
use crate::{
    CompositeOperation, FontRole, Rect, SemanticCommandPayload, SemanticCommandSource, StableId,
    TextRenderPlacementSource,
};

/// The commands of one element layer, appended in draw order. Each command id
/// derives from the element's source key, the command role and the number of
/// commands appended before it.
pub(super) struct LayerCommands<'a> {
    source_key: &'a str,
    layer_id: StableId,
    ordinal: u32,
    commands: &'a mut Vec<SemanticCommandSource>,
}

impl<'a> LayerCommands<'a> {
    pub(super) fn new(
        source_key: &'a str,
        layer_id: StableId,
        commands: &'a mut Vec<SemanticCommandSource>,
    ) -> Self {
        Self {
            source_key,
            layer_id,
            ordinal: 0,
            commands,
        }
    }

    fn next_id(&mut self, role: &str) -> StableId {
        let id = semantic_command_id(self.source_key, role, self.ordinal);
        self.ordinal += 1;
        id
    }

    fn push(&mut self, command: SemanticCommandSource) -> &mut SemanticCommandSource {
        self.commands.push(command);
        let last = self.commands.len() - 1;
        &mut self.commands[last]
    }

    /// Appends an image command that draws `descriptor` into `bounds`,
    /// sampling the `uv` rectangle of the image, or all of it when `uv` is
    /// `None`. Returns the appended command.
    pub(super) fn push_image(
        &mut self,
        role: &str,
        descriptor: &ResourceDescriptor,
        bounds: Rect,
        uv: Option<Rect>,
    ) -> &mut SemanticCommandSource {
        let mut command = SemanticCommandSource::image(
            self.next_id(role),
            self.layer_id,
            role,
            descriptor.resource.clone(),
            bounds,
        );
        command.hit_geometry = rect_quad(bounds);
        for (key, value) in &descriptor.provenance {
            command
                .metadata
                .insert(format!("resource_source.{key}"), value.clone());
        }
        if let (Some(uv), SemanticCommandPayload::Image { uv: target_uv, .. }) =
            (uv, &mut command.payload)
        {
            *target_uv = uv;
        }
        self.push(command)
    }

    /// Appends a composite command that applies `operation` over `bounds`.
    pub(super) fn push_composite(
        &mut self,
        role: &str,
        bounds: Rect,
        operation: CompositeOperation,
    ) {
        let mut command =
            SemanticCommandSource::composite(self.next_id(role), self.layer_id, role, bounds);
        if let SemanticCommandPayload::Composite {
            operation: target, ..
        } = &mut command.payload
        {
            *target = operation;
        }
        self.push(command);
    }

    /// Appends a single centered line with a scene-space font size, as used
    /// by General text nodes.
    pub(super) fn push_centered_profile_text(
        &mut self,
        role: &str,
        field: &str,
        value: String,
        bounds: Rect,
        font_size: f32,
    ) {
        let mut command = SemanticCommandSource::profile_text(
            self.next_id(role),
            self.layer_id,
            role,
            field,
            value,
            FontRole::RegionFontId(1),
        );
        command.bounds = bounds;
        command.matrix[4] = bounds.x + bounds.width / 2.0;
        command.matrix[5] = bounds.y + bounds.height / 2.0;
        command.hit_geometry = rect_quad(bounds);
        let text_scale = crate::tmp_text::PROFILE_FACE.scale;
        command.render_placement = Some(TextRenderPlacementSource {
            anchor_x: 0.0,
            baseline: Some(font_size * 0.35 / text_scale),
        });
        if let SemanticCommandPayload::Text {
            size, alignment, ..
        } = &mut command.payload
        {
            *size = font_size / text_scale;
            *alignment = 2;
        }
        self.push(command);
    }
}
