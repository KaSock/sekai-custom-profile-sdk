//! BOT media export over the existing NDJSON session.

use std::sync::Arc;

#[cfg(feature = "animation-export")]
use base64::Engine;
use sekai_profile::assets::AssetStore;
use sekai_profile::region::Region;
use sekai_profile::CustomProfileRenderer;
#[cfg(feature = "animation-export")]
use sekai_profile::RenderConfig;
use serde_json::{json, Value};

use crate::serve::AssetUrls;

pub fn capabilities() -> Value {
    json!({
        "animation_export": cfg!(feature = "animation-export"),
        "profile_media": true,
        "jpeg": cfg!(feature = "jpeg-turbo"),
    })
}

#[cfg(not(feature = "animation-export"))]
pub fn render_profile_media(
    _renderer: &CustomProfileRenderer,
    _assets: &Arc<AssetStore>,
    _urls: &AssetUrls,
    _request: &Value,
    _region: Region,
) -> Result<Value, String> {
    Err("rebuild sekai-profile-cli with --features animation-export".into())
}

#[cfg(feature = "animation-export")]
pub fn render_profile_media(
    renderer: &CustomProfileRenderer,
    assets: &Arc<AssetStore>,
    urls: &AssetUrls,
    request: &Value,
    region: Region,
) -> Result<Value, String> {
    use sekai_profile::animation::{
        resolve_preset, validate_magic, AnimationDocument, AnimationFormat,
    };
    use sekai_profile::profile::ProfileData;
    use sekai_profile::types::UserCustomProfileCard;

    let params = request.get("params").ok_or("missing params")?;
    let profile_json = params
        .get("profile")
        .filter(|value| value.is_object())
        .ok_or("params.profile must be a Profile API response")?;
    let mut pages: Vec<Value> = profile_json
        .get("userCustomProfileCards")
        .and_then(Value::as_array)
        .cloned()
        .ok_or("profile has no userCustomProfileCards array")?;
    if pages.is_empty() || pages.len() > 20 {
        return Err("expected 1 to 20 custom profile pages".into());
    }
    pages.sort_by_key(|page| page.get("seq").and_then(Value::as_i64).unwrap_or(i64::MAX));
    let quality = match params.get("quality") {
        None | Some(Value::Null) => 90,
        Some(value) => value
            .as_u64()
            .filter(|quality| (1..=100).contains(quality))
            .ok_or("quality must be an integer from 1 to 100")? as u8,
    };
    let preset_name = match params.get("preset") {
        None | Some(Value::Null) => "qq-v2",
        Some(value) => value.as_str().ok_or("preset must be a string")?,
    };
    let preset = resolve_preset(preset_name, Some(AnimationFormat::Mp4))
        .map_err(|error| error.to_string())?;
    let profile = ProfileData::from_json(profile_json);
    let config = RenderConfig::default();
    let mut items = Vec::new();
    let mut failures = Vec::new();
    let mut total_bytes = 0usize;
    let render = |raw_page: &Value| -> Result<MediaPage, String> {
        let page: UserCustomProfileCard = serde_json::from_value(raw_page.clone())
            .map_err(|error| format!("invalid page: {error}"))?;
        let card = &page.custom_profile_card;
        let document_key = format!(
            "bot-{}-request{}-page{}",
            region.as_str(),
            request.get("id").unwrap_or(&Value::Null),
            page.seq,
        );
        let plan = crate::asset_plan(renderer, card, Some(&profile), assets)?;
        crate::fetch_assets(assets, urls, &plan.fetch_keys(|key| assets.contains(key)));
        // Rebuild after fetching: optional frame choices and source dimensions
        // can depend on the now-available assets. Check recipe images too.
        let plan = crate::asset_plan(renderer, card, Some(&profile), assets)?;
        let missing = plan.missing_keys(|key| assets.contains(key));
        if !missing.is_empty() {
            return Err(format!("missing assets: {}", missing.join(", ")));
        }
        let mut warnings = renderer.validate_card(card);
        for key in plan.fetch_keys(|key| assets.contains(key)) {
            tracing::warn!(seq = page.seq, %key, "optional profile asset candidate unavailable");
            warnings.push(format!(
                "optional profile asset candidate unavailable: {key}"
            ));
        }
        let document = AnimationDocument {
            key: &document_key,
            region: region.as_str(),
        };
        let export = renderer
            .render_animation(card, Some(&profile), document, &preset, &config)
            .map_err(|error| error.to_string())?;
        let (kind, mime, data) = if export.animated {
            let encoded = export.encoded.ok_or("animated page has no artifact")?;
            validate_magic(encoded.format, &encoded.data).map_err(|error| error.to_string())?;
            ("video", "video/mp4", encoded.data)
        } else {
            let data = crate::render_page(
                renderer,
                card,
                Some(&profile),
                &config,
                crate::OutputFormat::Jpeg,
                quality,
            )?;
            ("image", "image/jpeg", data)
        };
        Ok(MediaPage {
            kind,
            mime,
            data,
            warnings,
        })
    };
    // Bound live animation exporters and temporary artifacts. Assemble each
    // wave in seq order so completion order cannot change the response budget.
    for batch in pages.chunks(page_workers()) {
        for (raw_page, result) in batch.iter().zip(render_parallel(batch, &render)) {
            let seq = raw_page.get("seq");
            let result = result.and_then(|page| {
                if total_bytes.saturating_add(page.data.len()) > 64 * 1024 * 1024 {
                    return Err("profile media exceeds the 64 MiB response budget".into());
                }
                total_bytes += page.data.len();
                Ok(json!({
                    "seq": seq, "kind": page.kind, "content_type": page.mime,
                    "bytes": page.data.len(),
                    "data": base64::engine::general_purpose::STANDARD.encode(&page.data),
                    "warnings": page.warnings,
                }))
            });
            match result {
                Ok(item) => items.push(item),
                Err(error) => {
                    tracing::warn!(seq = ?seq, %error, "profile page export failed");
                    failures.push(json!({"seq": seq, "error": error}));
                }
            }
        }
    }
    Ok(json!({"items": items, "failures": failures}))
}

#[cfg(feature = "animation-export")]
struct MediaPage {
    kind: &'static str,
    mime: &'static str,
    data: Vec<u8>,
    warnings: Vec<String>,
}

#[cfg(feature = "animation-export")]
fn page_workers() -> usize {
    std::env::var("SEKAI_PROFILE_PAGE_THREADS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|workers| (1..=16).contains(workers))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
                .min(4)
        })
}

#[cfg(feature = "animation-export")]
fn render_parallel<T: Sync, R: Send>(pages: &[T], render: &(impl Fn(&T) -> R + Sync)) -> Vec<R> {
    std::thread::scope(|scope| {
        let handles = pages
            .iter()
            .map(|page| scope.spawn(move || render(page)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("profile page worker panicked"))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "animation-export")]
    #[test]
    fn missing_overlay_texture_fails_before_rendering() {
        let mut md = sekai_profile::masterdata::JsonMasterDataProvider::empty();
        md.insert_table("cards", r#"[{"id":7,"assetbundleName":"fixture","cardRarityType":"rarity_4","attr":"cute","characterId":1}]"#).unwrap();
        let assets = Arc::new(AssetStore::new(1));
        let renderer = CustomProfileRenderer::new(Arc::new(md)).with_assets(Arc::clone(&assets));
        let raw_card = json!({"cardMembers": [{
            "objectData": {
                "layer": 0, "lock": false, "visible": true,
                "position": {"x": 0.0, "y": 0.0, "z": 0.0},
                "rotation": {"w": 1.0, "x": 0.0, "y": 0.0, "z": 0.0},
                "scale": {"x": 1.0, "y": 1.0, "z": 1.0}
            },
            "id": 7, "type": 1, "showMasterRank": true
        }]});
        let card = serde_json::from_value(raw_card.clone()).unwrap();
        let plan = crate::asset_plan(&renderer, &card, None, &assets).unwrap();
        let png = sekai_profile::codec::png::encode_rgba(1, 1, &[255; 4]).unwrap();
        for key in plan.fetch_keys(|key| key == "card/bg_base_wh") {
            assets.put(key, png.clone());
        }
        let result = render_profile_media(
            &renderer,
            &assets,
            &AssetUrls::default(),
            &json!({"params": {"profile": {"userCustomProfileCards": [{
                "seq": 1, "customProfileId": 1, "customProfileCardId": 1,
                "customProfileCard": raw_card
            }]}}}),
            Region::Cn,
        )
        .unwrap();
        assert!(result["items"].as_array().unwrap().is_empty());
        assert_eq!(
            result["failures"][0]["error"],
            "missing assets: card/bg_base_wh"
        );
    }

    #[cfg(feature = "animation-export")]
    #[test]
    fn pages_execute_concurrently_and_keep_results_in_input_order() {
        let barrier = std::sync::Barrier::new(4);
        let completed = std::sync::atomic::AtomicUsize::new(0);
        let results = render_parallel(&[1, 2, 3, 4], &|seq| {
            // All workers must start before any can finish: serial execution
            // cannot satisfy this barrier.
            barrier.wait();
            if *seq == 1 {
                while completed.load(std::sync::atomic::Ordering::Acquire) != 3 {
                    std::thread::yield_now();
                }
            } else {
                completed.fetch_add(1, std::sync::atomic::Ordering::Release);
            }
            if *seq == 2 {
                Err("bad page")
            } else {
                Ok(*seq)
            }
        });
        assert_eq!(results, [Ok(1), Err("bad page"), Ok(3), Ok(4)]);
    }

    #[cfg(all(feature = "animation-export", feature = "jpeg-turbo"))]
    #[test]
    fn static_pages_keep_order_and_isolate_bad_pages() {
        let assets = Arc::new(AssetStore::new(1));
        let renderer = CustomProfileRenderer::new(Arc::new(
            sekai_profile::masterdata::JsonMasterDataProvider::empty(),
        ))
        .with_assets(Arc::clone(&assets));
        let page = |seq| {
            json!({
                "seq": seq, "customProfileId": 1,
                "customProfileCardId": seq, "customProfileCard": {},
            })
        };
        let result = render_profile_media(
            &renderer,
            &assets,
            &AssetUrls::default(),
            &json!({"params": {"profile": {
                "userCustomProfileCards": [page(2), {"seq": 3}, page(1)],
            }}}),
            Region::Cn,
        )
        .unwrap();
        let items = result["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["seq"], 1);
        assert_eq!(items[1]["seq"], 2);
        assert_eq!(result["failures"].as_array().unwrap().len(), 1);
        assert_eq!(result["failures"][0]["seq"], 3);
        for item in items {
            assert_eq!(item["kind"], "image");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(item["data"].as_str().unwrap())
                .unwrap();
            assert!(bytes.starts_with(&[0xff, 0xd8, 0xff]));
        }
    }

    #[test]
    fn capabilities_report_build_features() {
        assert_eq!(
            capabilities()["animation_export"],
            cfg!(feature = "animation-export")
        );
        assert_eq!(capabilities()["jpeg"], cfg!(feature = "jpeg-turbo"));
    }

    #[cfg(feature = "animation-export")]
    #[test]
    fn malformed_profile_media_requests_are_rejected() {
        let renderer = CustomProfileRenderer::new(Arc::new(
            sekai_profile::masterdata::JsonMasterDataProvider::empty(),
        ));
        let assets = Arc::new(AssetStore::new(1));
        for params in [
            json!({}),
            json!({"profile": {}}),
            json!({"profile": {"userCustomProfileCards": []}}),
        ] {
            assert!(render_profile_media(
                &renderer,
                &assets,
                &AssetUrls::default(),
                &json!({"params": params}),
                Region::Cn,
            )
            .is_err());
        }
    }
}
