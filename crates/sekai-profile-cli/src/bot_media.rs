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
    for raw_page in pages {
        let page: UserCustomProfileCard = match serde_json::from_value(raw_page.clone()) {
            Ok(page) => page,
            Err(error) => {
                failures.push(
                    json!({"seq": raw_page.get("seq"), "error": format!("invalid page: {error}")}),
                );
                continue;
            }
        };
        let card = &page.custom_profile_card;
        let document_key = format!(
            "bot-{}-request{}-page{}",
            region.as_str(),
            request.get("id").unwrap_or(&Value::Null),
            page.seq,
        );
        let result = (|| -> Result<Value, String> {
            // General/card overlay resources are resolved from the scene. This
            // must also run for pages without a visible General element because
            // the regular card recipe can still reference static textures.
            let needed = || crate::missing_asset_keys(renderer, card, Some(&profile), assets);
            crate::fetch_assets(assets, urls, &needed());
            let missing = sekai_profile::asset_keys::missing_card_asset_keys_with_profile(
                card,
                Some(&profile),
                &renderer.masterdata(),
                |key| assets.contains(key),
            );
            // A nominal success with blank artwork is not a successful render.
            if !missing.is_empty() {
                return Err(format!("missing assets: {}", missing.join(", ")));
            }
            let mut warnings = renderer.validate_card(card);
            // The upstream profile helper includes every possible panel
            // resource, even if this page does not draw that panel. Fetch
            // these candidates, but do not fail an unrelated page for them.
            for key in needed() {
                tracing::warn!(seq = page.seq, %key, "profile panel asset unavailable");
                warnings.push(format!("profile panel asset unavailable: {key}"));
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
            if total_bytes.saturating_add(data.len()) > 64 * 1024 * 1024 {
                return Err("profile media exceeds the 64 MiB response budget".into());
            }
            total_bytes += data.len();
            Ok(json!({
                "seq": page.seq, "kind": kind, "content_type": mime,
                "bytes": data.len(),
                "data": base64::engine::general_purpose::STANDARD.encode(&data),
                "warnings": warnings,
            }))
        })();
        match result {
            Ok(item) => items.push(item),
            Err(error) => {
                tracing::warn!(seq = page.seq, %error, "profile page export failed");
                failures.push(json!({"seq": page.seq, "error": error}));
            }
        }
    }
    Ok(json!({"items": items, "failures": failures}))
}

#[cfg(test)]
mod tests {
    use super::*;

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
