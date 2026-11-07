//! Shared HTTP response helpers.

use lyquid::prelude::*;

pub(crate) fn json_error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

pub(crate) fn http_json_response(status: u16, body: &str) -> http::Response {
    http::Response {
        status,
        headers: vec![http::Header {
            name: "content-type".to_owned(),
            value: b"application/json".to_vec(),
        }],
        body: body.as_bytes().to_vec(),
    }
}
