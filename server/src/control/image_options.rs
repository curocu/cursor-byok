//! Applies temporary image-test choices without persisting model configuration.
use serde_json::{json, Map, Value};

use crate::{
    model::{ModelConfig, OPENAI_IMAGES_ENDPOINT},
    Error, Result,
};

pub fn apply(model: &mut ModelConfig, options: &Map<String, Value>) -> Result<()> {
    if options.is_empty() {
        return Ok(());
    }
    for (key, value) in options {
        let valid = match key.as_str() {
            "size" => value.as_str().is_some_and(|size| {
                size == "auto"
                    || size.split_once('x').is_some_and(|(w, h)| {
                        [w, h].into_iter().all(|s| {
                            !s.is_empty()
                                && s.bytes().all(|b| b.is_ascii_digit())
                                && s.parse::<u32>().is_ok_and(|n| n > 0 && n <= 8192)
                        })
                    })
            }),
            "quality" => value
                .as_str()
                .is_some_and(|s| ["auto", "low", "medium", "high", "xhigh", "max"].contains(&s)),
            "output_format" => value
                .as_str()
                .is_some_and(|s| ["png", "jpeg", "webp"].contains(&s)),
            "background" => value
                .as_str()
                .is_some_and(|s| ["auto", "opaque", "transparent"].contains(&s)),
            "output_compression" => value.as_u64().is_some_and(|n| n <= 100),
            "moderation" => value.as_str().is_some_and(|s| ["auto", "low"].contains(&s)),
            "complexity" => value
                .as_str()
                .is_some_and(|s| ["low", "medium", "high"].contains(&s)),
            _ => false,
        };
        if !valid {
            return Err(Error::Config(format!("invalid image test option: {key}")));
        }
    }
    let mut extra = model.extra_params().clone();
    let target = if model.openai_endpoint == OPENAI_IMAGES_ENDPOINT {
        &mut extra
    } else {
        if extra.get("tools").is_none() {
            extra["tools"] = json!([{"type":"image_generation"}]);
        }
        let tools = extra["tools"]
            .as_array_mut()
            .filter(|tools| tools.len() == 1 && tools[0]["type"] == "image_generation")
            .ok_or_else(|| {
                Error::Config(
                    "Responses image tools must contain exactly one image_generation tool".into(),
                )
            })?;
        &mut tools[0]
    };
    for (key, value) in options {
        target[key] = value.clone();
    }
    let format = target["output_format"].as_str().unwrap_or("png");
    if format == "jpeg" && target["background"] == "transparent" {
        return Err(Error::Config(
            "transparent backgrounds require PNG or WebP".into(),
        ));
    }
    if format == "png"
        && target
            .get("output_compression")
            .is_some_and(|value| !value.is_null())
    {
        return Err(Error::Config(
            "output_compression requires JPEG or WebP".into(),
        ));
    }
    model.openai_extra_params = extra;
    model.openai_extra_params_enabled = true;
    Ok(())
}
