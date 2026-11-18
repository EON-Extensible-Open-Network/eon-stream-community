// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Accounts, sessions, and what an account holds about itself.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::http::{ApiError, Out, Req, Res};
use crate::{token_hash, Ctx};

const SESSION_DAYS: i64 = 30;

fn valid_username(name: &str) -> bool {
    (3..=24).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn hash(password: &str) -> Result<String, ApiError> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| ApiError::invalid("no randomness"))?;
    let salt = SaltString::encode_b64(&bytes).map_err(|_| ApiError::invalid("no salt"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| ApiError::invalid("that password could not be kept"))
}

fn verify(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

fn new_token() -> Result<String, ApiError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| ApiError::invalid("no randomness"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn create(ctx: &Ctx, req: &Req) -> Out {
    let username = req.text("username").unwrap_or_default().to_lowercase();
    let email = req.text("email").unwrap_or_default();
    let password = req
        .body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !valid_username(&username) {
        return Err(ApiError::invalid(
            "a username is 3 to 24 letters, digits or underscores",
        ));
    }
    if !(3..=254).contains(&email.len()) || !email.contains('@') || email.contains(' ') {
        return Err(ApiError::invalid("that is not an e-mail address"));
    }
    if password.chars().count() < 10 {
        return Err(ApiError::invalid("a password is at least ten characters"));
    }
    let taken: bool = ctx.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1 OR email = ?2)",
        params![username, email],
        |r| r.get(0),
    )?;
    if taken {
        return Err(ApiError::taken("that username or e-mail is already in use"));
    }
    let hashed = hash(password)?;
    ctx.db.execute(
        "INSERT INTO users (username, email, hash, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![username, email, hashed, ctx.now],
    )?;
    let id = ctx.db.last_insert_rowid();
    // Every account has a watchlist, private until its owner says otherwise.
    ctx.db.execute(
        "INSERT INTO lists (owner_id, name, public, kind, created_at, updated_at)
         VALUES (?1, 'Watchlist', 0, 'watchlist', ?2, ?2)",
        params![id, ctx.now],
    )?;
    Ok(Res::created(json!({ "username": username })))
}

pub fn sign_in(ctx: &Ctx, req: &Req) -> Out {
    let login = req.text("login").unwrap_or_default();
    let password = req
        .body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let found: Option<(i64, String, String, String)> = ctx
        .db
        .query_row(
            "SELECT id, username, hash, role FROM users WHERE username = ?1 OR email = ?1",
            params![login],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    // The same answer for an unknown login and a wrong password.
    let Some((id, username, stored, role)) =
        found.filter(|(_, _, stored, _)| verify(password, stored))
    else {
        return Err(ApiError::new(
            401,
            "unauthorised",
            "that login and password do not match",
        ));
    };
    let _ = stored;
    let token = new_token()?;
    let expires = ctx.now + SESSION_DAYS * 86_400;
    ctx.db.execute(
        "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
        params![token_hash(&token), id, expires],
    )?;
    ctx.db.execute(
        "DELETE FROM sessions WHERE expires_at <= ?1",
        params![ctx.now],
    )?;
    Ok(Res::ok(json!({
        "token": token, "username": username, "role": role, "expires_at": expires
    })))
}

pub fn sign_out(ctx: &Ctx, req: &Req) -> Out {
    ctx.me()?;
    if let Some(token) = &req.token {
        ctx.db.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            params![token_hash(token)],
        )?;
    }
    Ok(Res::empty())
}

pub fn me(ctx: &Ctx) -> Out {
    let user = ctx.me()?;
    let row = ctx.db.query_row(
        "SELECT username, email, role, bio, created_at, suspended_until FROM users WHERE id = ?1",
        params![user.id],
        |r| {
            Ok(json!({
                "username": r.get::<_, String>(0)?,
                "email": r.get::<_, String>(1)?,
                "role": r.get::<_, String>(2)?,
                "bio": r.get::<_, Option<String>>(3)?,
                "created_at": r.get::<_, i64>(4)?,
                "suspended_until": r.get::<_, Option<i64>>(5)?,
            }))
        },
    )?;
    Ok(Res::ok(row))
}

pub fn update(ctx: &Ctx, req: &Req) -> Out {
    let user = ctx.writer()?;
    let bio = req.text("bio");
    if bio.as_ref().is_some_and(|b| b.chars().count() > 300) {
        return Err(ApiError::invalid("a bio is up to 300 characters"));
    }
    ctx.db.execute(
        "UPDATE users SET bio = ?1 WHERE id = ?2",
        params![bio, user.id],
    )?;
    me(ctx)
}

pub fn password(ctx: &Ctx, req: &Req) -> Out {
    let user = ctx.me()?;
    let old = req
        .body
        .get("old")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let new = req
        .body
        .get("new")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stored: String = ctx.db.query_row(
        "SELECT hash FROM users WHERE id = ?1",
        params![user.id],
        |r| r.get(0),
    )?;
    if !verify(old, &stored) {
        return Err(ApiError::new(
            401,
            "unauthorised",
            "the current password is not right",
        ));
    }
    if new.chars().count() < 10 {
        return Err(ApiError::invalid("a password is at least ten characters"));
    }
    ctx.db.execute(
        "UPDATE users SET hash = ?1 WHERE id = ?2",
        params![hash(new)?, user.id],
    )?;
    // Every session ends: a changed password is often a stolen one.
    ctx.db
        .execute("DELETE FROM sessions WHERE user_id = ?1", params![user.id])?;
    Ok(Res::empty())
}

fn rows(ctx: &Ctx, sql: &str, id: i64) -> Result<Vec<Value>, ApiError> {
    let mut statement = ctx.db.prepare(sql)?;
    let count = statement.column_count();
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    let out = statement
        .query_map(params![id], |row| {
            let mut object = serde_json::Map::new();
            for (at, name) in names.iter().enumerate().take(count) {
                let value = match row.get_ref(at)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => json!(n),
                    rusqlite::types::ValueRef::Real(n) => json!(n),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(_) => Value::Null,
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// Everything held about the account, in one document (madde 31).
pub fn export(ctx: &Ctx) -> Out {
    let user = ctx.me()?;
    let id = user.id;
    Ok(Res::ok(json!({
        "account": rows(ctx, "SELECT username, email, role, bio, created_at, suspended_until FROM users WHERE id = ?1", id)?,
        "ratings": rows(ctx, "SELECT type, id, score, review, spoiler, at FROM ratings WHERE user_id = ?1", id)?,
        "helpful_votes": rows(ctx, "SELECT (SELECT username FROM users WHERE id = author_id) AS author, type, id, helpful FROM helpful WHERE voter_id = ?1", id)?,
        "facts": rows(ctx, "SELECT subject_kind, subject, kind, text, spoiler, state, at FROM facts WHERE author_id = ?1", id)?,
        "fact_votes": rows(ctx, "SELECT fact_id, interesting FROM fact_votes WHERE user_id = ?1", id)?,
        "person_edits": rows(ctx, "SELECT person_id, field, value, state, at FROM person_edits WHERE author_id = ?1", id)?,
        "parental_votes": rows(ctx, "SELECT type, id, category, level FROM parental WHERE user_id = ?1", id)?,
        "segments": rows(ctx, "SELECT type, item, kind, start, end, at FROM segments WHERE author_id = ?1", id)?,
        "lists": rows(ctx, "SELECT id, name, description, public, ranked, kind, created_at, updated_at FROM lists WHERE owner_id = ?1", id)?,
        "list_items": rows(ctx, "SELECT li.list_id, li.type, li.id, li.note, li.position, li.at FROM list_items li JOIN lists l ON l.id = li.list_id WHERE l.owner_id = ?1", id)?,
        "list_likes": rows(ctx, "SELECT list_id FROM list_likes WHERE user_id = ?1", id)?,
        "nominations": rows(ctx, "SELECT event_id, year, category, type, title_id, person, won, state, at FROM nominations WHERE author_id = ?1", id)?,
        "polls": rows(ctx, "SELECT id, question, state, at FROM polls WHERE author_id = ?1", id)?,
        "poll_votes": rows(ctx, "SELECT poll_id, idx FROM poll_votes WHERE user_id = ?1", id)?,
        "friends": rows(ctx, "SELECT (SELECT username FROM users WHERE id = to_id) AS asked, at FROM friends WHERE from_id = ?1", id)?,
        "blocks": rows(ctx, "SELECT (SELECT username FROM users WHERE id = blocked_id) AS blocked FROM blocks WHERE user_id = ?1", id)?,
        "reports": rows(ctx, "SELECT kind, target, reason, state, at FROM reports WHERE reporter_id = ?1", id)?,
        "appeals": rows(ctx, "SELECT log_id, text, state, at FROM appeals WHERE user_id = ?1", id)?,
        "notifications": rows(ctx, "SELECT kind, body, at, read FROM notifications WHERE user_id = ?1", id)?,
        "sessions": rows(ctx, "SELECT expires_at FROM sessions WHERE user_id = ?1", id)?,
    })))
}

/// The account and everything it wrote, gone. The moderation log keeps that
/// decisions were made, not what they were about.
pub fn delete(ctx: &Ctx) -> Out {
    let user = ctx.me()?;
    ctx.db
        .execute("DELETE FROM users WHERE id = ?1", params![user.id])?;
    Ok(Res::empty())
}

pub fn notifications(ctx: &Ctx) -> Out {
    let user = ctx.me()?;
    let mut statement = ctx.db.prepare(
        "SELECT id, kind, body, at, read FROM notifications WHERE user_id = ?1 ORDER BY id DESC LIMIT 50",
    )?;
    let items = statement
        .query_map(params![user.id], |r| {
            let body: String = r.get(2)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "kind": r.get::<_, String>(1)?,
                "body": serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
                "at": r.get::<_, i64>(3)?,
                "read": r.get::<_, i64>(4)? != 0,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let unread: i64 = ctx.db.query_row(
        "SELECT COUNT(*) FROM notifications WHERE user_id = ?1 AND read = 0",
        params![user.id],
        |r| r.get(0),
    )?;
    Ok(Res::ok(json!({ "items": items, "unread": unread })))
}

pub fn read_notifications(ctx: &Ctx, req: &Req) -> Out {
    let user = ctx.me()?;
    match req.body.get("read") {
        Some(Value::String(all)) if all == "all" => {
            ctx.db.execute(
                "UPDATE notifications SET read = 1 WHERE user_id = ?1",
                params![user.id],
            )?;
        }
        Some(Value::Array(ids)) => {
            for id in ids.iter().filter_map(Value::as_i64) {
                ctx.db.execute(
                    "UPDATE notifications SET read = 1 WHERE user_id = ?1 AND id = ?2",
                    params![user.id, id],
                )?;
            }
        }
        _ => return Err(ApiError::invalid("say which notifications were read")),
    }
    Ok(Res::empty())
}

#[cfg(test)]
mod tests {
    use super::{hash, valid_username, verify};

    #[test]
    fn a_username_is_lowercase_letters_digits_and_underscores() {
        assert!(valid_username("ayse_1"));
        assert!(!valid_username("Ayse"));
        assert!(!valid_username("ab"));
        assert!(!valid_username("a b c"));
    }

    #[test]
    fn a_password_is_kept_only_as_a_hash() {
        let stored = hash("correct horse battery").unwrap_or_default();
        assert!(!stored.contains("correct"));
        assert!(verify("correct horse battery", &stored));
        assert!(!verify("wrong horse battery", &stored));
    }
}
