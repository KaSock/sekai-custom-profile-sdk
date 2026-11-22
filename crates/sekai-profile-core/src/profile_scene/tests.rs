use std::collections::BTreeMap;

use super::*;
use crate::SemanticCommandPayload;

fn text_scene(text_type: i32) -> ResolvedProfileScene {
    let card = serde_json::from_value(serde_json::json!({
        "texts": [{
            "objectData": {
                "layer": 0,
                "lock": false,
                "visible": true,
                "position": { "x": 12.0, "y": 24.0, "z": 0.0 },
                "rotation": { "w": 1.0, "x": 0.0, "y": 0.0, "z": 0.0 },
                "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
            },
            "colorId": 1,
            "fontId": 1,
            "lineSpacing": 0.0,
            "outlineColorId": 2,
            "outlineSize": 0.0,
            "size": 24.0,
            "text": "Left default",
            "type": text_type
        }]
    }))
    .unwrap();
    let snapshot = ProfileResolveSnapshot {
        fonts: BTreeMap::from([(1, "RegionFont".into())]),
        colors: BTreeMap::from([(1, [1.0; 4]), (2, [0.0; 4])]),
        ..ProfileResolveSnapshot::default()
    };
    resolve_profile_scene(&card, "text-alignment", &snapshot).unwrap()
}

#[test]
fn missing_horizontal_alignment_resolves_to_the_explicit_left_scene() {
    let left = text_scene(1);
    assert_eq!(left.commands.len(), 1);
    for text_type in [0, 8, 512, 1024] {
        assert_eq!(text_scene(text_type), left, "text_type={text_type}");
    }
}

#[test]
fn nonzero_horizontal_flags_are_preserved_when_vertical_flags_are_removed() {
    for alignment in 1..=7 {
        let scene = text_scene(512 | alignment);
        assert_eq!(scene, text_scene(alignment));
        assert!(matches!(
            scene.commands[0].payload,
            SemanticCommandPayload::Text { alignment: value, .. }
                if i32::from(value) == alignment
        ));
    }
}

#[test]
fn placed_live_master_progress_uses_scroll_placement_and_tmp_units() {
    let layer_id = StableId(1);
    for (full_size, width, progress, expected_x, expected_size) in [
        (true, 380.0, 309, 78.0, 9.0),
        (false, 180.0, i32::MAX, 0.0, 3.2),
    ] {
        let honor = HonorVisualSnapshot {
            source_field: "honors".into(),
            source_id: "3013".into(),
            honor_id: 3013,
            honor_level: 1,
            full_size,
            visual: HonorVisualKind::Standard {
                honor_type: "achievement".into(),
                has_star: false,
                is_live_master: true,
                progress,
                background: None,
                frame_candidates: Vec::new(),
                overlay: None,
                star: None,
                star_high: None,
                live_star_on: None,
                live_star_off: None,
            },
        };
        let mut commands = Vec::new();
        lower_honor_visual(
            &mut LayerCommands::new("live-master", layer_id, &mut commands),
            &honor,
            crate::profile_layout::ElementLayout {
                cx: 0.0,
                cy: 0.0,
                w: width,
                h: 80.0,
            },
        )
        .unwrap();
        let [command] = commands.as_slice() else {
            panic!("expected only the progress text command");
        };
        assert_eq!(command.matrix[4], expected_x);
        let placement = command.render_placement.unwrap();
        let baseline = command.matrix[5] + placement.baseline.unwrap() * 2.0;
        assert!((baseline - 28.0).abs() < 0.001);
        assert!(matches!(
            &command.payload,
            SemanticCommandPayload::Text {
                source: crate::TextSource::ProfileField { value, .. },
                size,
                alignment: 2,
                max_width: None,
                ..
            } if value == &progress.to_string() && *size == expected_size
        ));
    }
}
