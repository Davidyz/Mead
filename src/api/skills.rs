//! Skill HTTP endpoint helpers and local catalog persistence.

use std::sync::Arc;

use lyquid::prelude::*;

use crate::skill::{AcquiredSkills, Skill, SkillError, SkillManager};
use crate::types::{invalid_input, parse_hash};

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

#[derive(serde::Deserialize)]
struct SkillGetRequest {
    digest: String,
}

/// Parses the `digest` from a `/skills/get` JSON request body.
pub(crate) fn parse_skill_get_request(body: Option<&[u8]>) -> Result<Hash, String> {
    let body = body.ok_or_else(|| "request body is required".to_owned())?;
    let request: SkillGetRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid skill get request JSON: {error}"))?;
    let digest = request.digest.trim();
    if digest.is_empty() {
        return Err("digest must not be empty".to_owned());
    }
    parse_hash(digest).map_err(|error| format!("invalid digest: {error}"))
}

/// Serves the full body of a skill by digest.
///
/// Peer-provided skills are served from the per-call cache or the peer bodies
/// persisted in instance state when results arrive during a search UPC. Locally
/// uploaded skills fall back to the node-local catalog. The body uses the same
/// JSON skill structure accepted by `POST /skills/upload`.
pub(crate) fn handle_skill_get(
    ctx: &mut crate::__lyquid::InstanceContext,
    req: &http::Request,
) -> LyquidResult<http::Response> {
    let digest = match parse_skill_get_request(req.body.as_deref()) {
        Ok(digest) => digest,
        Err(message) => return Ok(http_json_response(400, &json_error(&message))),
    };

    if let Some(skill) = AcquiredSkills::shared().get(&digest) {
        return skill_json_response(&skill);
    }

    let acquired_blob = {
        let blobs = ctx.instance.acquired_skill_blobs.read();
        blobs.get(&digest).cloned()
    };
    if let Some(bytes) = acquired_blob {
        let skill = match Skill::from_canonical_bytes(&bytes) {
            Ok(skill) => skill,
            Err(error) => {
                return Ok(http_json_response(500, &json_error(&error.to_string())));
            }
        };
        AcquiredSkills::shared().record(digest, &skill);
        return skill_json_response(&skill);
    }

    ensure_skills_loaded(ctx)?;
    let Some(skill) = SkillManager::shared()
        .ok()
        .and_then(|manager| manager.get(&digest))
    else {
        return Ok(http_json_response(404, &json_error("skill not found")));
    };

    skill_json_response(&skill)
}

fn skill_json_response(skill: &Skill) -> LyquidResult<http::Response> {
    match skill.to_json() {
        Ok(body) => Ok(http_json_response(200, &body)),
        Err(error) => Ok(http_json_response(500, &json_error(&error.to_string()))),
    }
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
pub(crate) fn ensure_skills_loaded(ctx: &crate::__lyquid::InstanceContext) -> LyquidResult<()> {
    let blobs = ctx.instance.skill_blobs.read();
    ensure_skills_loaded_from(&blobs)
}

pub(crate) fn ensure_skills_loaded_immutable(
    ctx: &crate::__lyquid::ImmutableInstanceContext,
) -> LyquidResult<()> {
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
