// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The reference server for EON Stream's community API.
//!
//! `eon-stream-spec/docs/community-api.md` is the contract; this crate is one
//! way to meet it, small enough to read in an afternoon. It is written to be
//! run by whoever chooses to operate a community -- not by the EON project,
//! which may not run one before madde 30's Faz L1 gate is met. The operator
//! of a server is the data controller and the hosting provider, with those
//! obligations (madde 11-14, 31).

pub mod api;
pub mod db;
pub mod http;

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::http::{ApiError, Req, Res};

/// Who is asking, when they signed in.
#[derive(Clone, Debug)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub suspended_until: Option<i64>,
}

/// What a handler works with.
pub struct Ctx<'a> {
    pub db: &'a Connection,
    pub now: i64,
    pub user: Option<User>,
}

impl Ctx<'_> {
    /// The signed-in user.
    pub fn me(&self) -> Result<&User, ApiError> {
        self.user.as_ref().ok_or_else(ApiError::unauthorised)
    }

    /// The signed-in user, allowed to write: not suspended.
    pub fn writer(&self) -> Result<&User, ApiError> {
        let user = self.me()?;
        if user.suspended_until.is_some_and(|until| until > self.now) {
            return Err(ApiError::new(
                403,
                "suspended",
                "this account is suspended and can read, but not write",
            ));
        }
        Ok(user)
    }

    pub fn moderator(&self) -> Result<&User, ApiError> {
        let user = self.me()?;
        if user.role == "moderator" || user.role == "admin" {
            Ok(user)
        } else {
            Err(ApiError::forbidden("this needs a moderator"))
        }
    }

    pub fn admin(&self) -> Result<&User, ApiError> {
        let user = self.me()?;
        if user.role == "admin" {
            Ok(user)
        } else {
            Err(ApiError::forbidden("this needs an administrator"))
        }
    }

    #[must_use]
    pub fn uid(&self) -> Option<i64> {
        self.user.as_ref().map(|u| u.id)
    }
}

#[must_use]
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A token's stored form.
#[must_use]
pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn user_for(db: &Connection, token: &str, now: i64) -> rusqlite::Result<Option<User>> {
    db.query_row(
        "SELECT u.id, u.username, u.role, u.suspended_until FROM sessions s
         JOIN users u ON u.id = s.user_id WHERE s.token_hash = ?1 AND s.expires_at > ?2",
        params![token_hash(token), now],
        |row| {
            Ok(User {
                id: row.get(0)?,
                username: row.get(1)?,
                role: row.get(2)?,
                suspended_until: row.get(3)?,
            })
        },
    )
    .optional()
}

/// Requests per minute from one address, and account or sign-in attempts per
/// hour: enough for a person, too few for a script guessing passwords.
const PER_MINUTE: u32 = 240;
const ACCOUNT_PER_HOUR: u32 = 20;

#[derive(Default)]
struct Limits {
    minute: HashMap<String, (Instant, u32)>,
    hour: HashMap<String, (Instant, u32)>,
}

impl Limits {
    fn allow(&mut self, peer: &str, sensitive: bool) -> bool {
        let now = Instant::now();
        let bump = |map: &mut HashMap<String, (Instant, u32)>, window: Duration, max: u32| {
            let slot = map.entry(peer.to_owned()).or_insert((now, 0));
            if now.duration_since(slot.0) > window {
                *slot = (now, 0);
            }
            slot.1 += 1;
            slot.1 <= max
        };
        let ok = bump(&mut self.minute, Duration::from_secs(60), PER_MINUTE);
        if self.minute.len() > 50_000 {
            self.minute.clear();
        }
        if sensitive {
            let fine = bump(&mut self.hour, Duration::from_secs(3600), ACCOUNT_PER_HOUR);
            if self.hour.len() > 50_000 {
                self.hour.clear();
            }
            return ok && fine;
        }
        ok
    }
}

/// Answer one request.
pub fn handle(db: &Connection, req: &Req) -> Res {
    let now = now();
    let user = match &req.token {
        Some(token) => match user_for(db, token, now) {
            Ok(user) => user,
            Err(error) => return ApiError::from(error).into_res(),
        },
        None => None,
    };
    let ctx = Ctx { db, now, user };
    match api::route(&ctx, req) {
        Ok(res) => res,
        Err(error) => error.into_res(),
    }
}

/// Serve on a listening server until it is closed.
pub fn serve(server: &tiny_http::Server, db: &Connection) {
    let mut limits = Limits::default();
    for mut request in server.incoming_requests() {
        let res = match Req::read(&mut request) {
            Ok(req) => {
                let sensitive = req.method == "POST"
                    && matches!(
                        req.parts.get(1).map(String::as_str),
                        Some("accounts" | "sessions")
                    );
                if req.peer == http::LOCAL || limits.allow(&req.peer, sensitive) {
                    handle(db, &req)
                } else {
                    ApiError::new(429, "rate_limited", "too many requests; wait a minute")
                        .into_res()
                }
            }
            Err(error) => error.into_res(),
        };
        let body = res.body.map(|b| b.to_string()).unwrap_or_default();
        let mut response = tiny_http::Response::from_string(body).with_status_code(res.status);
        if res.status != 204 {
            if let Ok(header) =
                tiny_http::Header::from_bytes("Content-Type", "application/json; charset=utf-8")
            {
                response.add_header(header);
            }
        }
        let _ = request.respond(response);
    }
}

// ------------------------------------------------------------------ shared

/// A content type the contract allows.
pub fn check_type(kind: &str) -> Result<(), ApiError> {
    if matches!(kind, "movie" | "series" | "channel" | "tv" | "anime") {
        Ok(())
    } else {
        Err(ApiError::invalid("unknown type"))
    }
}

/// An addon's id: letters, digits and `_:.-`, up to 120.
pub fn check_id(id: &str) -> Result<(), ApiError> {
    if !id.is_empty()
        && id.len() <= 120
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_:.-".contains(c))
    {
        Ok(())
    } else {
        Err(ApiError::invalid("that is not a title id"))
    }
}

/// A person's id from their name: lower case, letters and digits, hyphens.
#[must_use]
pub fn person_id(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        let c = match c {
            'ç' => 'c',
            'ğ' => 'g',
            'ı' | 'î' | 'í' | 'ì' | 'ï' => 'i',
            'ö' | 'ô' | 'ó' | 'ò' | 'ø' => 'o',
            'ş' => 's',
            'ü' | 'û' | 'ú' | 'ù' => 'u',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'á' | 'à' | 'â' | 'ä' | 'å' | 'ã' => 'a',
            'ñ' => 'n',
            other => other,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(80).collect()
}

fn short(value: Option<&Value>, max: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|s| s.trim().chars().take(max).collect::<String>())
        .filter(|s| !s.is_empty())
}

fn words(value: Option<&Value>, max: usize) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| short(Some(v), 120))
                .take(max)
                .collect()
        })
        .unwrap_or_default()
}

/// Keep what a client said a title is: its name, year, poster address,
/// genres, and the people it named -- the catalogue the server's pages are
/// drawn from. Later descriptions replace earlier ones; nothing is fetched.
pub fn remember_title(
    db: &Connection,
    kind: &str,
    id: &str,
    title: &Value,
) -> Result<(), ApiError> {
    let Some(name) = short(title.get("name"), 300) else {
        // Nothing to say about it; a title already known keeps its name.
        let known: Option<i64> = db
            .query_row(
                "SELECT 1 FROM titles WHERE type = ?1 AND id = ?2",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()?;
        return if known.is_some() {
            Ok(())
        } else {
            Err(ApiError::invalid("a title needs its name"))
        };
    };
    let year = short(title.get("year"), 20);
    let poster = short(title.get("poster"), 600).filter(|p| p.starts_with("https://"));
    let genres = words(title.get("genres"), 12);
    db.execute(
        "INSERT INTO titles (type, id, name, year, poster, genres) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(type, id) DO UPDATE SET name = excluded.name,
           year = COALESCE(excluded.year, titles.year), poster = COALESCE(excluded.poster, titles.poster),
           genres = CASE WHEN excluded.genres = '[]' THEN titles.genres ELSE excluded.genres END",
        params![kind, id, name, year, poster, serde_json::to_string(&genres).unwrap_or_default()],
    )?;
    for (role, key) in [("cast", "cast"), ("director", "director")] {
        for person in words(title.get(key), 30) {
            let pid = person_id(&person);
            if pid.is_empty() {
                continue;
            }
            db.execute(
                "INSERT INTO people (id, name) VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING",
                params![pid, person],
            )?;
            db.execute(
                "INSERT OR IGNORE INTO credits (type, id, person_id, role) VALUES (?1, ?2, ?3, ?4)",
                params![kind, id, pid, role],
            )?;
        }
    }
    Ok(())
}

/// A title as pages show it.
pub fn title_json(db: &Connection, kind: &str, id: &str) -> rusqlite::Result<Value> {
    Ok(db
        .query_row(
            "SELECT name, year, poster, genres FROM titles WHERE type = ?1 AND id = ?2",
            params![kind, id],
            |row| {
                let genres: String = row.get(3)?;
                Ok(serde_json::json!({
                    "name": row.get::<_, String>(0)?,
                    "year": row.get::<_, Option<String>>(1)?,
                    "poster": row.get::<_, Option<String>>(2)?,
                    "genres": serde_json::from_str::<Value>(&genres).unwrap_or(Value::Null),
                }))
            },
        )
        .optional()?
        .unwrap_or_else(|| serde_json::json!({ "name": id })))
}

pub fn user_id(db: &Connection, username: &str) -> Result<i64, ApiError> {
    db.query_row(
        "SELECT id FROM users WHERE username = ?1",
        params![username],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(ApiError::not_found)
}

pub fn username(db: &Connection, id: i64) -> rusqlite::Result<String> {
    db.query_row(
        "SELECT username FROM users WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map(|name| name.unwrap_or_else(|| "[deleted]".to_owned()))
}

/// Tell a user something happened.
pub fn notify(
    db: &Connection,
    user: i64,
    kind: &str,
    body: &Value,
    now: i64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO notifications (user_id, kind, body, at) VALUES (?1, ?2, ?3, ?4)",
        params![user, kind, body.to_string(), now],
    )?;
    // The newest two hundred are kept.
    db.execute(
        "DELETE FROM notifications WHERE user_id = ?1 AND id NOT IN
         (SELECT id FROM notifications WHERE user_id = ?1 ORDER BY id DESC LIMIT 200)",
        params![user],
    )?;
    Ok(())
}

/// Whether `a` blocked `b`, either way round.
pub fn blocked(db: &Connection, a: i64, b: i64) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM blocks WHERE (user_id = ?1 AND blocked_id = ?2) OR (user_id = ?2 AND blocked_id = ?1))",
        params![a, b],
        |r| r.get(0),
    )
}

/// Twenty a page, and whether there is another.
pub const PAGE: i64 = 20;

#[must_use]
pub fn paged(mut items: Vec<Value>, size: i64) -> (Vec<Value>, bool) {
    let more = items.len() as i64 > size;
    items.truncate(size as usize);
    (items, more)
}

#[cfg(test)]
mod tests {
    use super::person_id;

    #[test]
    fn a_name_folds_to_one_id() {
        assert_eq!(person_id("Bryan Cranston"), "bryan-cranston");
        assert_eq!(person_id("  Şener  Şen "), "sener-sen");
        assert_eq!(person_id("Chloë Grace Moretz"), "chloe-grace-moretz");
        assert_eq!(person_id("!!!"), "");
    }
}
