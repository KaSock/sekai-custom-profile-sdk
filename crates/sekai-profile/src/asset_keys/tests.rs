use super::{key_to_s3_path, logical_path_to_asset_key};

const PREFIX: &str = "assets/cn/";

fn render_plan_masterdata() -> crate::masterdata::MasterData {
    let mut provider = crate::masterdata::JsonMasterDataProvider::empty();
    provider.insert_table("cards", r#"[{"id":7,"assetbundleName":"fixture","cardRarityType":"rarity_4","attr":"cute","characterId":1}]"#).expect("cards");
    provider
        .insert_table(
            "customProfileTextFonts",
            r#"[{"id":1,"fontName":"fixture","name":"fixture"}]"#,
        )
        .expect("fonts");
    crate::masterdata::MasterData::new(std::sync::Arc::new(provider))
}

fn plan_object() -> serde_json::Value {
    serde_json::json!({
        "layer": 0, "lock": false, "visible": true,
        "position": {"x": 0.0, "y": 0.0, "z": 0.0},
        "rotation": {"w": 1.0, "x": 0.0, "y": 0.0, "z": 0.0},
        "scale": {"x": 1.0, "y": 1.0, "z": 1.0}
    })
}

fn honor_plan_masterdata() -> crate::masterdata::MasterData {
    let mut provider = crate::masterdata::JsonMasterDataProvider::empty();
    let rows = [
        (6622, "event", "honor_top_001000", "honor_bg_event_forlight"),
        (
            3381,
            "event",
            "honor_top_002000",
            "honor_bg_event_underwater",
        ),
        (
            3452,
            "event",
            "honor_top_001000_event_underwater_cp4",
            "honor_bg_event_underwater_cp4",
        ),
        (
            6103,
            "event",
            "honor_top_002000",
            "honor_bg_event_wl_2nd_schoolrefusal",
        ),
        (5023, "rank_match", "common/tier_23", "season_2024_autumn"),
        (5390, "rank_match", "common/tier_23", "season_2025_winter"),
        (5818, "rank_match", "common/tier_25", "season_2025_spring"),
        (3021, "achievement", "honor_3021", ""),
    ];
    let honors = rows.map(|(id, _, bundle, _)| {
        serde_json::json!({
            "id": id, "groupId": id, "honorRarity": "high", "assetbundleName": bundle,
            "levels": [{"level": 1}]
        })
    });
    let groups = rows.map(|(id, kind, _, background)| {
        serde_json::json!({
            "id": id, "honorType": kind, "backgroundAssetbundleName": background
        })
    });
    provider
        .insert_table("honors", &serde_json::to_string(&honors).unwrap())
        .unwrap();
    provider
        .insert_table("honorGroups", &serde_json::to_string(&groups).unwrap())
        .unwrap();
    provider
        .insert_table(
            "customProfileTextFonts",
            r#"[{"id":1,"fontName":"fixture","name":"fixture"}]"#,
        )
        .unwrap();
    crate::masterdata::MasterData::new(std::sync::Arc::new(provider))
}

#[test]
fn placed_honor_overlays_are_downloaded_even_when_they_are_optional() {
    let md = honor_plan_masterdata();
    for id in [3381, 3452, 6103, 5023, 5390, 5818] {
        for full_size in [true, false] {
            let assets = crate::assets::AssetStore::new(1);
            let card = serde_json::from_value(serde_json::json!({
                "honors": [{"objectData": plan_object(), "id": id, "fullSize": full_size}]
            }))
            .unwrap();
            let honor = md.resolve_honor(id, 1).unwrap().asset_plan(full_size);
            let overlay = honor.overlay.unwrap().key;
            let plan = super::RenderAssetPlan::build(&card, None, &md, &assets).unwrap();
            let keys = plan.fetch_keys(|_| false);
            assert!(keys.contains(&overlay), "missing download: {overlay}");
            // An unavailable overlay must still allow export of the remaining layers.
            let png = crate::codec::png::encode_rgba(1, 1, &[255; 4]).unwrap();
            for key in keys.iter().filter(|key| **key != overlay) {
                assets.put(key.clone(), png.clone());
            }
            let plan = super::RenderAssetPlan::build(&card, None, &md, &assets).unwrap();
            assert!(plan.missing_keys(|key| assets.contains(key)).is_empty());
            assert_eq!(
                plan.fetch_keys(|key| assets.contains(key)),
                [overlay.clone()]
            );
            assets.put(overlay.clone(), png);
            let scene = crate::semantic_resolve::resolve_card_commands_with_profile(
                &card,
                &md,
                "downloaded-honor",
                None,
                "cn",
                Some(&assets),
            )
            .unwrap();
            assert!(
                scene.commands.iter().any(|command| matches!(
                    &command.payload,
                    sekai_profile_core::SemanticCommandPayload::Image { resource, .. }
                        if resource.key == overlay
                )),
                "downloaded overlay was not drawn: {overlay}"
            );
        }
    }
}

#[test]
fn general_honors_download_the_sizes_used_by_the_rendered_slots() {
    let md = honor_plan_masterdata();
    let assets = crate::assets::AssetStore::new(1);
    let card = serde_json::from_value(serde_json::json!({
        "generals": [{"objectData": plan_object(), "type": 6}]
    }))
    .unwrap();
    let profile = crate::profile::ProfileData::from_json(&serde_json::json!({
        "userHonors": [
            {"honorId": 6622, "level": 1},
            {"honorId": 5390, "level": 1},
            {"honorId": 3021, "level": 6}
        ],
        "userProfileHonors": [
            {"seq": 1, "honorId": 6622, "honorLevel": 1},
            {"seq": 2, "honorId": 5390, "honorLevel": 1},
            {"seq": 3, "honorId": 3021, "honorLevel": 6}
        ]
    }));
    let expected = [
        "honor/honor_bg_event_forlight/degree_main",
        "honor/honor_top_001000/rank_main",
        "rank_live/honor/season_2025_winter/degree_sub",
        "rank_live/honor/common/tier_23/sub",
        "honor/honor_3021/degree_sub",
    ];
    let plan = super::RenderAssetPlan::build(&card, Some(&profile), &md, &assets).unwrap();
    let keys = plan.fetch_keys(|_| false);
    for key in expected {
        assert!(
            keys.iter().any(|candidate| candidate == key),
            "missing download: {key}"
        );
    }
    let png = crate::codec::png::encode_rgba(1, 1, &[255; 4]).unwrap();
    for key in keys {
        assets.put(key, png.clone());
    }
    let plan = super::RenderAssetPlan::build(&card, Some(&profile), &md, &assets).unwrap();
    assert!(plan.fetch_keys(|key| assets.contains(key)).is_empty());
    assert!(plan.missing_keys(|key| assets.contains(key)).is_empty());
    let scene = crate::semantic_resolve::resolve_card_commands_with_profile(
        &card,
        &md,
        "downloaded-honor-panel",
        Some(&profile),
        "en",
        Some(&assets),
    )
    .unwrap();
    for key in expected {
        assert!(
            scene.commands.iter().any(|command| matches!(
                &command.payload,
                sekai_profile_core::SemanticCommandPayload::Image { resource, .. }
                    if resource.key == key
            )),
            "downloaded honor layer was not drawn: {key}"
        );
    }
}

#[test]
fn cropped_card_overlay_plans_its_textures_without_a_general_panel() {
    let md = render_plan_masterdata();
    let assets = crate::assets::AssetStore::new(1);
    let mut card: crate::types::CustomProfileCard = serde_json::from_value(serde_json::json!({
        "cardMembers": [{"objectData": plan_object(), "id": 7, "type": 1, "showMasterRank": true}]
    }))
    .expect("card");
    let key = "card/bg_base_wh";
    let plan = super::RenderAssetPlan::build(&card, None, &md, &assets).expect("plan");
    let keys = plan.fetch_keys(|_| false);
    assert!(keys.iter().any(|key| key == "card/bg_base_wh"));
    assert!(keys.iter().any(|key| key == "card/cardFrame_M_4"));
    assert!(keys.iter().any(|key| key == "card/icon_attribute_cute_64"));
    let png = crate::codec::png::encode_rgba(1, 1, &[255; 4]).expect("PNG");
    for candidate in keys.iter().filter(|candidate| candidate.as_str() != key) {
        assets.put(candidate.clone(), png.clone());
    }
    let plan = super::RenderAssetPlan::build(&card, None, &md, &assets).expect("updated plan");
    assert_eq!(plan.missing_keys(|key| assets.contains(key)), [key]);
    assets.put(key.into(), png);
    assert!(plan.missing_keys(|key| assets.contains(key)).is_empty());

    card.card_members[0].show_master_rank = Some(false);
    let plan = super::RenderAssetPlan::build(&card, None, &md, &assets).expect("image-only plan");
    assert!(!plan
        .fetch_keys(|_| false)
        .iter()
        .any(|candidate| candidate == key));
    card.card_members[0].show_master_rank = Some(true);
    card.card_members[0].object_data.visible = false;
    let plan = super::RenderAssetPlan::build(&card, None, &md, &assets).expect("hidden plan");
    assert!(plan.fetch_keys(|_| false).is_empty());
}

#[test]
fn general_deck_uses_the_same_overlay_resource_plan() {
    let md = render_plan_masterdata();
    let assets = crate::assets::AssetStore::new(1);
    let card: crate::types::CustomProfileCard = serde_json::from_value(serde_json::json!({
        "generals": [{"objectData": plan_object(), "type": 3}]
    }))
    .expect("card");
    let profile = crate::profile::ProfileData {
        deck_members: vec![crate::profile::DeckMember {
            card_id: 7,
            level: 60,
            ..Default::default()
        }],
        ..Default::default()
    };
    let plan = super::RenderAssetPlan::build(&card, Some(&profile), &md, &assets).expect("plan");
    assert!(plan
        .fetch_keys(|_| false)
        .iter()
        .any(|key| key == "card/bg_base_wh"));
    assert_eq!(
        plan.missing_keys(|key| key != "card/bg_base_wh"),
        ["card/bg_base_wh"]
    );
    // Profile helper candidates for panels absent from this page remain optional.
    assert!(plan
        .missing_keys(|key| key != "sprite/icon/icon_playerRank")
        .is_empty());
}

#[test]
fn empty_pages_do_not_request_overlay_or_unused_profile_textures() {
    let md = render_plan_masterdata();
    let assets = crate::assets::AssetStore::new(1);
    let card: crate::types::CustomProfileCard =
        serde_json::from_value(serde_json::json!({})).expect("card");
    let profile = crate::profile::ProfileData::default();
    let plan = super::RenderAssetPlan::build(&card, Some(&profile), &md, &assets).expect("plan");
    assert!(plan.fetch_keys(|_| false).is_empty());
    assert!(plan.missing_keys(|_| false).is_empty());
}

#[test]
fn resource_planning_reports_scene_errors() {
    let md = crate::masterdata::MasterData::new(std::sync::Arc::new(
        crate::masterdata::JsonMasterDataProvider::empty(),
    ));
    let assets = crate::assets::AssetStore::new(1);
    let card: crate::types::CustomProfileCard = serde_json::from_value(serde_json::json!({
        "generals": [{"objectData": plan_object(), "type": 3}]
    }))
    .expect("card");
    let profile = crate::profile::ProfileData::default();
    assert!(matches!(
        super::RenderAssetPlan::build(&card, Some(&profile), &md, &assets),
        Err(crate::semantic_resolve::ResolveError::MissingRegionFont(1))
    ));
}

#[test]
fn game_assets_resolve_under_the_prefix() {
    assert_eq!(
        key_to_s3_path("character/member_small/abn/card_normal", PREFIX),
        "assets/cn/character/member_small/abn/card_normal.png"
    );
}

#[test]
fn indexed_bonds_honor_paths_restore_runtime_keys() {
    for key in [
        "bonds_honor/chr_sd_01_01",
        "bonds_honor/word/honorname_0102_01_01",
        "honor/example/degree_main",
    ] {
        let logical_path = key_to_s3_path(key, "");
        assert_eq!(
            logical_path_to_asset_key(&logical_path).as_deref(),
            Some(key)
        );
    }
}
struct PairHonorProvider;

impl crate::masterdata::MasterDataProvider for PairHonorProvider {
    fn resolve_story_banner(&self, _: &str, _: i32) -> Option<String> {
        None
    }
    fn get_card(&self, _: i32) -> Option<crate::types::CardEntry> {
        None
    }
    fn resolve_color(&self, _: i32) -> Option<crate::masterdata::ResolvedColor> {
        None
    }
    fn resolve_font(&self, _: i32) -> Option<String> {
        None
    }
    fn resolve_stamp(&self, _: i32) -> Option<String> {
        None
    }
    fn resolve_resource(&self, _: &str, _: i32) -> Option<crate::masterdata::ResourceInfo> {
        None
    }
    fn resolve_honor(&self, _: i32, _: i32) -> Option<crate::masterdata::ResolvedHonor> {
        None
    }
    fn get_bonds_honor(&self, id: i32) -> Option<crate::types::BondsHonorEntry> {
        Some(crate::types::BondsHonorEntry {
            id,
            game_character_unit_id1: 21,
            game_character_unit_id2: 1,
            honor_rarity: "middle".to_string(),
            configurable_unit_virtual_singer: true,
        })
    }
    fn get_bonds_honor_word(&self, id: i64) -> Option<crate::types::BondsHonorWordEntry> {
        Some(crate::types::BondsHonorWordEntry {
            id: id as i32,
            assetbundle_name: "honorname_0121_01".to_string(),
            bonds_group_id: 1,
            seq: 1,
        })
    }
    fn get_honor(&self, _: i32) -> Option<crate::types::HonorEntry> {
        None
    }
    fn resolve_unit_vs_sd(&self, self_id: i32, partner_id: i32) -> i32 {
        match (self_id, partner_id) {
            (21, 1) => 27,
            // Only reachable when the partner was already substituted.
            (1, 27) => 99,
            _ => self_id,
        }
    }
    fn font_count(&self) -> usize {
        0
    }
    fn color_count(&self) -> usize {
        0
    }
}

#[test]
fn card_honors_follow_the_levels_the_player_owns() {
    let card: crate::types::CustomProfileCard = serde_json::from_value(serde_json::json!({
        "bondsHonors": [
            {
                "objectData": {
                    "layer": 0, "lock": false, "visible": true,
                    "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
                    "rotation": { "w": 1.0, "x": 0.0, "y": 0.0, "z": 0.0 },
                    "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
                },
                "id": 1, "wordId": 5, "fullSize": true, "inverse": false,
                "useUnitVirtualSinger": false
            }
        ]
    }))
    .expect("card fixture");
    let md = crate::masterdata::MasterData::new(std::sync::Arc::new(PairHonorProvider));
    let unowned = crate::profile::ProfileData::from_json(&serde_json::json!({
        "userHonors": [],
        "userBondsHonors": [{ "bondsHonorId": 2, "level": 3 }]
    }));
    assert!(
        super::missing_card_asset_keys_with_profile(&card, Some(&unowned), &md, |_| false)
            .is_empty()
    );
    let owned = crate::profile::ProfileData::from_json(&serde_json::json!({
        "userBondsHonors": [{ "bondsHonorId": 1, "level": 3 }]
    }));
    assert!(
        super::missing_card_asset_keys_with_profile(&card, Some(&owned), &md, |_| false)
            .contains(&"honor/bonds/21".to_string())
    );
}

#[test]
fn hidden_bonds_honors_request_no_assets() {
    let card: crate::types::CustomProfileCard = serde_json::from_value(serde_json::json!({
        "bondsHonors": [{
            "objectData": {
                "layer": 0, "lock": false, "visible": false,
                "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
                "rotation": { "w": 1.0, "x": 0.0, "y": 0.0, "z": 0.0 },
                "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
            },
            "id": 1, "wordId": 5, "fullSize": true, "inverse": false,
            "useUnitVirtualSinger": false
        }]
    }))
    .expect("card fixture");
    let md = crate::masterdata::MasterData::new(std::sync::Arc::new(PairHonorProvider));
    assert!(super::collect_card_asset_keys(&card, &md).is_empty());
    assert!(super::missing_card_asset_keys(&card, &md, |_| false).is_empty());
}

#[test]
fn card_bonds_honors_request_every_layer_of_the_game_art() {
    let card: crate::types::CustomProfileCard = serde_json::from_value(serde_json::json!({
        "bondsHonors": [{
            "objectData": {
                "layer": 0, "lock": false, "visible": true,
                "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
                "rotation": { "w": 1.0, "x": 0.0, "y": 0.0, "z": 0.0 },
                "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
            },
            "id": 1, "wordId": 5, "fullSize": true, "inverse": false,
            "useUnitVirtualSinger": true
        }]
    }))
    .expect("card fixture");
    let md = crate::masterdata::MasterData::new(std::sync::Arc::new(PairHonorProvider));
    let mut keys = super::collect_card_asset_keys(&card, &md);
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "bonds_honor/chr_sd_01_01",
            "bonds_honor/chr_sd_27_01",
            "bonds_honor/word/honorname_0121_01_02",
            "honor/bonds/1",
            "honor/bonds/21",
            "honor/frame_degree_m_2",
            "honor/icon_degreeLv",
            "honor/icon_degreeLv6",
            "honor/mask_degree_main",
        ]
    );
}

/// A character honor whose masterdata row carries neither a background
/// bundle nor a frame still has to produce a downloadable key. The
/// background falls back to the honor's own bundle name, so no key may
/// collapse into an empty path segment.
#[test]
fn a_character_honor_without_background_or_frame_keeps_every_path_segment() {
    use crate::masterdata::{
        MasterData, MasterDataProvider, ResolvedColor, ResolvedHonor, ResourceInfo,
    };
    use crate::types::{BondsHonorEntry, BondsHonorWordEntry, CardEntry, HonorEntry};

    struct BackgroundlessHonorProvider;

    impl MasterDataProvider for BackgroundlessHonorProvider {
        fn resolve_story_banner(&self, _: &str, _: i32) -> Option<String> {
            None
        }
        fn get_card(&self, _: i32) -> Option<CardEntry> {
            None
        }
        fn resolve_color(&self, _: i32) -> Option<ResolvedColor> {
            None
        }
        fn resolve_font(&self, _: i32) -> Option<String> {
            None
        }
        fn resolve_stamp(&self, _: i32) -> Option<String> {
            None
        }
        fn resolve_resource(&self, _: &str, _: i32) -> Option<ResourceInfo> {
            None
        }
        fn resolve_honor(&self, _: i32, honor_level: i32) -> Option<ResolvedHonor> {
            Some(ResolvedHonor {
                asset_bundle_name: "honor_0001".to_string(),
                honor_rarity: "high".to_string(),
                honor_type: "character".to_string(),
                background_asset_bundle_name: None,
                frame_name: None,
                is_live_master: false,
                has_star: true,
                honor_level,
                honor_mission_type: None,
            })
        }
        fn get_bonds_honor(&self, _: i32) -> Option<BondsHonorEntry> {
            None
        }
        fn get_bonds_honor_word(&self, _: i64) -> Option<BondsHonorWordEntry> {
            None
        }
        fn get_honor(&self, _: i32) -> Option<HonorEntry> {
            None
        }
        fn resolve_unit_vs_sd(&self, self_id: i32, _: i32) -> i32 {
            self_id
        }
        fn font_count(&self) -> usize {
            0
        }
        fn color_count(&self) -> usize {
            0
        }
    }

    let md = MasterData::new(std::sync::Arc::new(BackgroundlessHonorProvider));
    let mut requirements = super::AssetRequirements::default();
    super::collect_honor_keys(1, 1, false, &md, &mut requirements);
    let keys = requirements.keys();

    let background = keys
        .iter()
        .find(|key| key.contains("degree_sub"))
        .expect("a background key");
    assert_eq!(background, "honor/honor_0001/degree_sub");
    for key in &keys {
        assert!(!key.contains("//"), "key has an empty path segment: {key}");
    }
}
