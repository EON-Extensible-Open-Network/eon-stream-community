// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Requests in, answers out: the contract's transport rules in one place.

use std::collections::HashMap;
use std::io::Read;

use serde_json::{json, Value};

/// The most a request body may be. A review is 5000 characters; nothing in
/// the contract comes near this.
const MAX_BODY: u64 = 64 * 1024;

/// A request, read and checked.
pub struct Req {
    pub method: String,
    pub parts: Vec<String>,
    pub query: HashMap<String, String>,
    pub body: Value,
    pub token: Option<String>,
    pub peer: String,
}

impl Req {
    /// Read a request off the wire.
    ///
    /// # Errors
    ///
    /// A body that is too large or is not JSON.
    pub fn read(request: &mut tiny_http::Request) -> Result<Self, ApiError> {
        let url = request.url().to_owned();
        let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
        let parts = path
            .split('/')
            .filter(|p| !p.is_empty())
            .map(decode)
            .collect();
        let query = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(k), decode(v))
            })
            .collect();
        let token = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .and_then(|h| {
                h.value
                    .as_str()
                    .strip_prefix("Bearer ")
                    .map(ToOwned::to_owned)
            });
        let mut text = String::new();
        request
            .as_reader()
            .take(MAX_BODY + 1)
            .read_to_string(&mut text)
            .map_err(|_| ApiError::invalid("the body is not UTF-8 text"))?;
        if text.len() as u64 > MAX_BODY {
            return Err(ApiError::new(413, "invalid", "the body is too large"));
        }
        let body = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(|_| ApiError::invalid("the body is not JSON"))?
        };
        Ok(Self {
            method: request.method().as_str().to_owned(),
            parts,
            query,
            body,
            token,
            peer: peer_of(request),
        })
    }

    /// The `?page=` number, from 0.
    #[must_use]
    pub fn page(&self) -> i64 {
        self.query
            .get("page")
            .and_then(|p| p.parse::<i64>().ok())
            .unwrap_or(0)
            .clamp(0, 10_000)
    }

    #[must_use]
    pub fn q(&self, key: &str) -> Option<&str> {
        self.query
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// A string field of the body.
    #[must_use]
    pub fn text(&self, key: &str) -> Option<String> {
        self.body
            .get(key)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    #[must_use]
    pub fn flag(&self, key: &str) -> bool {
        self.body.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
}

/// Who is asking, for the rate limit. Behind the TLS proxy the README asks
/// for, every request arrives from this machine, so the proxy's
/// `X-Forwarded-For` is believed -- only from a loopback peer, where only the
/// operator's proxy can be. A loopback peer with no such header is the
/// operator themselves, and is answered as `local`.
fn peer_of(request: &tiny_http::Request) -> String {
    let Some(address) = request.remote_addr() else {
        return String::new();
    };
    if !address.ip().is_loopback() {
        return address.ip().to_string();
    }
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Forwarded-For"))
        .and_then(|h| {
            h.value
                .as_str()
                .rsplit(',')
                .next()
                .map(|ip| ip.trim().to_owned())
        })
        .filter(|ip| !ip.is_empty())
        .unwrap_or_else(|| LOCAL.to_owned())
}

/// The operator, on the machine the server runs on: not rate limited.
pub const LOCAL: &str = "local";

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[at + 1..=at + 2]).unwrap_or("");
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    at += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            other => out.push(other),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An answer: a status and a JSON body (none for 204).
pub struct Res {
    pub status: u16,
    pub body: Option<Value>,
}

impl Res {
    #[must_use]
    pub fn ok(body: Value) -> Self {
        Self {
            status: 200,
            body: Some(body),
        }
    }

    #[must_use]
    pub fn created(body: Value) -> Self {
        Self {
            status: 201,
            body: Some(body),
        }
    }

    #[must_use]
    pub fn accepted(body: Value) -> Self {
        Self {
            status: 202,
            body: Some(body),
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            status: 204,
            body: None,
        }
    }
}

/// The contract's error: a status, a code and a sentence.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    #[must_use]
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, "invalid", message)
    }
    #[must_use]
    pub fn unauthorised() -> Self {
        Self::new(401, "unauthorised", "sign in first")
    }
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, "forbidden", message)
    }
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(404, "not_found", "there is nothing here")
    }
    #[must_use]
    pub fn taken(message: impl Into<String>) -> Self {
        Self::new(409, "taken", message)
    }

    #[must_use]
    pub fn into_res(self) -> Res {
        Res {
            status: self.status,
            body: Some(json!({ "error": self.code, "message": self.message })),
        }
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        // Logged for the operator; the client is told only that it failed.
        eprintln!("database: {error}");
        Self::new(500, "invalid", "the server could not do that")
    }
}

pub type Out = Result<Res, ApiError>;

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn a_path_and_a_query_are_percent_decoded() {
        assert_eq!(decode("bryan%20cranston"), "bryan cranston");
        assert_eq!(decode("a+b"), "a b");
        assert_eq!(decode("tt0903747%3A1%3A1"), "tt0903747:1:1");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
