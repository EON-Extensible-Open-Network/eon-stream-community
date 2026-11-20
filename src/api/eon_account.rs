// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Signing in with EON Account (plan madde 46, community-api v0.1).
//!
//! The viewer's application asks their account server for a five-minute
//! assertion addressed to this server, and hands it over. This server checks
//! it against the account server's key -- which the operator gave it, so no
//! request leaves this server -- and learns a pseudonym that is its own (no
//! other service sees the same one) and, if the viewer chose, their
//! username. It never sees a password, an e-mail address or a session of the
//! account server. An account made this way has no password and no e-mail
//! here.

use std::sync::OnceLock;

use eon_account_client::assertion::{self, KeySet};
use rusqlite::{params, OptionalExtension};
use serde_json::json;

use crate::http::{ApiError, Out, Req, Res};
use crate::Ctx;

/// What this server accepts: its own origin, and the account servers it trusts.
pub struct Trust {
    pub origin: String,
    pub issuers: Vec<KeySet>,
}

static TRUST: OnceLock<Trust> = OnceLock::new();

/// Set once, at start. Without it, signing in with EON Account is refused.
pub fn configure(trust: Trust) -> bool {
    TRUST.set(trust).is_ok()
}

/// The names this server allows, made from an EON username: dots and dashes
/// become underscores, and it is cut to 24 characters.
fn local_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(24)
        .collect()
}

pub fn sign_in(ctx: &Ctx, req: &Req) -> Out {
    let refused = || ApiError::new(401, "unauthorised", "that assertion is not accepted here");
    let trust = TRUST.get().ok_or_else(|| {
        ApiError::new(403, "forbidden", "this server does not accept EON Account")
    })?;
    let text = req.text("assertion").ok_or_else(refused)?;
    let now = u64::try_from(ctx.now).unwrap_or(0);
    let claims = trust
        .issuers
        .iter()
        .find_map(|keys| assertion::verify(&text, keys, &trust.origin, now).ok())
        .ok_or_else(refused)?;

    let linked: Option<i64> = ctx
        .db
        .query_row(
            "SELECT user_id FROM account_links WHERE issuer = ?1 AND subject = ?2",
            params![claims.iss, claims.sub],
            |r| r.get(0),
        )
        .optional()?;
    let id = if let Some(id) = linked {
        id
    } else {
        {
            // A name for the newcomer: theirs if they shared it and it is free,
            // otherwise one made from the pseudonym.
            let base = claims
                .name
                .as_deref()
                .map(local_name)
                .filter(|n| n.len() >= 3)
                .unwrap_or_else(|| format!("viewer_{}", local_name(&claims.sub[..8])));
            let mut name = base.clone();
            for n in 2..1000 {
                let taken: bool = ctx.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1)",
                    params![name],
                    |r| r.get(0),
                )?;
                if !taken {
                    break;
                }
                let suffix = n.to_string();
                name = format!("{}{suffix}", &base[..base.len().min(24 - suffix.len())]);
            }
            ctx.db.execute(
                "INSERT INTO users (username, email, hash, created_at) VALUES (?1, NULL, NULL, ?2)",
                params![name, ctx.now],
            )?;
            let id = ctx.db.last_insert_rowid();
            ctx.db.execute(
                "INSERT INTO lists (owner_id, name, public, kind, created_at, updated_at)
                 VALUES (?1, 'Watchlist', 0, 'watchlist', ?2, ?2)",
                params![id, ctx.now],
            )?;
            ctx.db.execute(
                "INSERT INTO account_links (issuer, subject, user_id) VALUES (?1, ?2, ?3)",
                params![claims.iss, claims.sub, id],
            )?;
            id
        }
    };
    let (username, role): (String, String) = ctx.db.query_row(
        "SELECT username, role FROM users WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (token, expires) = super::accounts::new_session(ctx, id)?;
    Ok(Res::ok(json!({
        "token": token, "username": username, "role": role, "expires_at": expires
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_eon_username_becomes_a_name_this_server_allows() {
        assert_eq!(local_name("deniz.k-2"), "deniz_k_2");
        assert_eq!(local_name(&"a".repeat(32)).len(), 24);
    }
}
