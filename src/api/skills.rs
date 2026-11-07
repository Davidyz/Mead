//! Skill HTTP endpoint helpers and local catalog persistence.

use std::sync::Arc;

use lyquid::prelude::*;

use crate::skill::{Skill, SkillError, SkillManager};
use crate::types::invalid_input;

use super::http::{http_json_response, json_error};

pub(crate) fn skill_count_response() -> LyquidResult<http::Response> {
    let count = SkillManager::shared()
        .map(|manager| manager.len())
        .unwrap_or_default();
    lyquid::println!("mead: skill count requested: count={count}");
    Ok(skill_count_http_response(count))
}

pub(crate) fn skill_count_http_response(count: usize) -> http::Response {
    let body = serde_json::json!({ "count": count }).to_string();
    http_json_response(200, &body)
}

pub(crate) fn skill_list_json(manager: &SkillManager) -> String {
    let skills: Vec<_> = manager
        .iter()
        .map(|(digest, skill)| {
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "digest": digest.to_string(),
            })
        })
        .collect();
    serde_json::json!(skills).to_string()
}

pub(crate) fn handle_skill_upload(
    ctx: &mut crate::__lyquid::InstanceContext,
    req: http::Request,
) -> LyquidResult<http::Response> {
    let (skill, digest) = match parse_uploaded_skill(req.body.as_deref()) {
        Ok(skill) => skill,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    if let Err(error) = register_uploaded_skill(&skill) {
        return Ok(http_json_response(500, &json_error(&error.to_string())));
    }

    let digest_hash = match skill.digest() {
        Ok(digest) => digest,
        Err(error) => return Ok(http_json_response(500, &json_error(&error.to_string()))),
    };
    let bytes = match skill.canonical_bytes() {
        Ok(bytes) => bytes,
        Err(error) => return Ok(http_json_response(500, &json_error(&error.to_string()))),
    };
    ctx.instance.skill_blobs.write().insert(digest_hash, bytes);

    lyquid::println!(
        "mead: accepted skill name={:?} description={:?} digest={digest}",
        skill.name,
        skill.description
    );

    let body = serde_json::json!({
        "uploaded": 1,
        "digests": [digest],
    })
    .to_string();
    Ok(http_json_response(200, &body))
}

pub(crate) fn parse_uploaded_skill(body: Option<&[u8]>) -> Result<(Skill, String), String> {
    let body = body.ok_or_else(|| "request body is required".to_owned())?;
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| format!("invalid skill JSON: {error}"))?;
    if !value.is_object() {
        return Err("request body must be a JSON object representing one skill".to_owned());
    }

    let skill = match serde_json::from_value::<Skill>(value.clone()) {
        Ok(skill) => skill,
        Err(direct_error) => {
            let encoded = serde_json::to_string(&value)
                .map_err(|error| format!("failed to re-encode skill: {error}"))?;
            Skill::from_json(&encoded).map_err(|error| {
                format!("skill is invalid (serde: {direct_error}; transport: {error})")
            })?
        }
    };

    let digest = skill
        .digest()
        .map_err(|error| format!("skill is invalid: {error}"))?
        .to_string();
    Ok((skill, digest))
}

// Rebuilds the in-memory catalog from the canonical bytes persisted in
// instance state. The process-wide manager does not survive a Lyquid VM
// restart, but instance state does.
pub(crate) fn ensure_skills_loaded(ctx: &mut crate::__lyquid::InstanceContext) -> LyquidResult<()> {
    let blobs = ctx.instance.skill_blobs.read();
    ensure_skills_loaded_from(&blobs)
}

pub(crate) fn ensure_skills_loaded_from(blobs: &HashMap<Hash, Vec<u8>>) -> LyquidResult<()> {
    if blobs.is_empty() {
        return Ok(());
    }

    if SkillManager::shared().is_err() {
        SkillManager::init_shared(Vec::<Skill>::new())
            .map_err(|error| invalid_input(error.to_string()))?;
    }
    let manager = SkillManager::shared().map_err(|error| invalid_input(error.to_string()))?;
    for (digest, bytes) in blobs {
        if manager.contains(digest) {
            continue;
        }
        let skill =
            Skill::from_canonical_bytes(bytes).map_err(|error| invalid_input(error.to_string()))?;
        manager
            .append(skill)
            .map_err(|error| invalid_input(error.to_string()))?;
    }
    Ok(())
}

pub(crate) fn register_uploaded_skill(skill: &Skill) -> Result<(), SkillError> {
    let manager = match SkillManager::shared() {
        Ok(manager) => Arc::clone(manager),
        Err(SkillError::NotInitialized) => match SkillManager::init_shared(Vec::new()) {
            Ok(manager) => manager,
            Err(SkillError::AlreadyInitialized) => Arc::clone(SkillManager::shared()?),
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    };

    manager.append(skill.clone())
}
