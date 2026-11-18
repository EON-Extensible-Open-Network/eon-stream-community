// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! People: finding them, their public page, friends, the feed, blocking.

use rusqlite::params;
use serde_json::{json, Value};

use crate::http::{ApiError, Out, Req, Res};
use crate::{blocked, notify, title_json, user_id, Ctx};

fn asked(ctx: &Ctx, from: i64, to: i64) -> rusqlite::Result<bool> {
    ctx.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM friends WHERE from_id = ?1 AND to_id = ?2)",
        params![from, to],
        |r| r.get(0),
    )
}

fn state(ctx: &Ctx, me: i64, other: i64) -> rusqlite::Result<&'static str> {
    Ok(match (asked(ctx, me, other)?, asked(ctx, other, me)?) {
        (true, true) => "friends",
        (true, false) => "asked",
        (false, true) => "incoming",
        (false, false) => "none",
    })
}

fn rating_json(ctx: &Ctx, r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let kind: String = r.get(0)?;
    let id: String = r.get(1)?;
    Ok(json!({
        "type": kind, "id": id, "title": title_json(ctx.db, &kind, &id)?,
        "score": r.get::<_, i64>(2)?, "review": r.get::<_, Option<String>>(3)?,
        "spoiler": r.get::<_, bool>(4)?, "at": r.get::<_, i64>(5)?,
    }))
}

pub fn find(ctx: &Ctx, req: &Req) -> Out {
    let q = req.q("q").unwrap_or_default().to_lowercase();
    if q.is_empty() {
        return Ok(Res::ok(json!([])));
    }
    let pattern = format!("{}%", q.replace(['%', '_'], ""));
    let mut statement = ctx.db.prepare(
        "SELECT username, (SELECT COUNT(*) FROM ratings r WHERE r.user_id = users.id) FROM users
         WHERE username LIKE ?1 ORDER BY username LIMIT 20",
    )?;
    let found = statement
        .query_map(params![pattern], |r| {
            Ok(json!({ "username": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Res::ok(Value::Array(found)))
}

pub fn profile(ctx: &Ctx, name: &str) -> Out {
    let id = user_id(ctx.db, name)?;
    let (username, bio): (String, Option<String>) = ctx.db.query_row(
        "SELECT username, bio FROM users WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut histogram = [0_i64; 10];
    let mut statement = ctx.db.prepare(
        "SELECT score, COUNT(*) FROM ratings WHERE user_id = ?1 AND hidden = 0 GROUP BY score",
    )?;
    for row in statement.query_map(params![id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (score, count) = row?;
        if (1..=10).contains(&score) {
            histogram[(score - 1) as usize] = count;
        }
    }
    let count: i64 = histogram.iter().sum();
    let total: i64 = histogram
        .iter()
        .enumerate()
        .map(|(at, n)| (at as i64 + 1) * n)
        .sum();
    let mut statement = ctx.db.prepare(
        "SELECT type, id, score, review, spoiler, at FROM ratings WHERE user_id = ?1 AND hidden = 0 ORDER BY at DESC LIMIT 40",
    )?;
    let ratings = statement
        .query_map(params![id], |r| rating_json(ctx, r))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statement = ctx.db.prepare(
        "SELECT id, name, (SELECT COUNT(*) FROM list_items i WHERE i.list_id = lists.id) FROM lists
         WHERE owner_id = ?1 AND public = 1 AND hidden = 0 ORDER BY updated_at DESC LIMIT 20",
    )?;
    let lists = statement
        .query_map(params![id], |r| Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "count": r.get::<_, i64>(2)? })))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let friend = match ctx.uid() {
        Some(me) if me != id => Value::String(state(ctx, me, id)?.to_owned()),
        Some(_) => Value::String("self".to_owned()),
        None => Value::Null,
    };
    Ok(Res::ok(json!({
        "username": username, "bio": bio, "count": count,
        "average": (count > 0).then(|| (total as f64 / count as f64 * 10.0).round() / 10.0),
        "histogram": histogram, "ratings": ratings, "lists": lists, "friend": friend,
    })))
}

pub fn friends(ctx: &Ctx) -> Out {
    let me = ctx.me()?.id;
    let names = |sql: &str| -> rusqlite::Result<Vec<String>> {
        let mut statement = ctx.db.prepare(sql)?;
        let out = statement.query_map(params![me], |r| r.get(0))?.collect();
        out
    };
    Ok(Res::ok(json!({
        "friends": names("SELECT u.username FROM friends a JOIN friends b ON b.from_id = a.to_id AND b.to_id = a.from_id JOIN users u ON u.id = a.to_id WHERE a.from_id = ?1 ORDER BY u.username")?,
        "incoming": names("SELECT u.username FROM friends a JOIN users u ON u.id = a.from_id WHERE a.to_id = ?1 AND NOT EXISTS (SELECT 1 FROM friends b WHERE b.from_id = ?1 AND b.to_id = a.from_id) ORDER BY a.at DESC")?,
        "outgoing": names("SELECT u.username FROM friends a JOIN users u ON u.id = a.to_id WHERE a.from_id = ?1 AND NOT EXISTS (SELECT 1 FROM friends b WHERE b.from_id = a.to_id AND b.to_id = ?1) ORDER BY a.at DESC")?,
    })))
}

pub fn befriend(ctx: &Ctx, name: &str) -> Out {
    let me = ctx.writer()?;
    let other = user_id(ctx.db, name)?;
    if other == me.id {
        return Err(ApiError::invalid("you cannot befriend yourself"));
    }
    if blocked(ctx.db, me.id, other)? {
        // Said as "not found", so a block is not announced.
        return Err(ApiError::not_found());
    }
    ctx.db.execute(
        "INSERT OR IGNORE INTO friends (from_id, to_id, at) VALUES (?1, ?2, ?3)",
        params![me.id, other, ctx.now],
    )?;
    let now = state(ctx, me.id, other)?;
    notify(
        ctx.db,
        other,
        "friend",
        &json!({ "username": me.username, "state": now }),
        ctx.now,
    )?;
    Ok(Res::ok(
        json!({ "state": if now == "friends" { "friends" } else { "asked" } }),
    ))
}

pub fn unfriend(ctx: &Ctx, name: &str) -> Out {
    let me = ctx.me()?.id;
    let other = user_id(ctx.db, name)?;
    ctx.db.execute(
        "DELETE FROM friends WHERE (from_id = ?1 AND to_id = ?2) OR (from_id = ?2 AND to_id = ?1)",
        params![me, other],
    )?;
    Ok(Res::empty())
}

/// What friends scored and listed lately.
pub fn feed(ctx: &Ctx) -> Out {
    let me = ctx.me()?.id;
    let friends = "SELECT a.to_id FROM friends a JOIN friends b ON b.from_id = a.to_id AND b.to_id = a.from_id WHERE a.from_id = ?1";
    let mut statement = ctx.db.prepare(&format!(
        "SELECT u.username, r.type, r.id, r.score, r.review, r.spoiler, r.at FROM ratings r JOIN users u ON u.id = r.user_id
         WHERE r.user_id IN ({friends}) AND r.hidden = 0 ORDER BY r.at DESC LIMIT 50"
    ))?;
    let mut items = statement
        .query_map(params![me], |r| {
            let kind: String = r.get(1)?;
            let id: String = r.get(2)?;
            Ok(json!({
                "username": r.get::<_, String>(0)?, "kind": "rating", "at": r.get::<_, i64>(6)?,
                "rating": {
                    "type": kind, "id": id, "title": title_json(ctx.db, &kind, &id)?,
                    "score": r.get::<_, i64>(3)?, "review": r.get::<_, Option<String>>(4)?,
                    "spoiler": r.get::<_, bool>(5)?, "at": r.get::<_, i64>(6)?,
                },
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statement = ctx.db.prepare(&format!(
        "SELECT u.username, l.id, l.name, l.updated_at FROM lists l JOIN users u ON u.id = l.owner_id
         WHERE l.owner_id IN ({friends}) AND l.public = 1 AND l.hidden = 0 ORDER BY l.updated_at DESC LIMIT 20"
    ))?;
    items.extend(
        statement
            .query_map(params![me], |r| {
                Ok(json!({
                    "username": r.get::<_, String>(0)?, "kind": "list", "at": r.get::<_, i64>(3)?,
                    "list": { "id": r.get::<_, i64>(1)?, "name": r.get::<_, String>(2)? },
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    items.sort_by(|a, b| b["at"].as_i64().cmp(&a["at"].as_i64()));
    items.truncate(50);
    Ok(Res::ok(Value::Array(items)))
}

pub fn block(ctx: &Ctx, name: &str, set: bool) -> Out {
    let me = ctx.me()?.id;
    let other = user_id(ctx.db, name)?;
    if other == me {
        return Err(ApiError::invalid("you cannot block yourself"));
    }
    if set {
        ctx.db.execute(
            "INSERT OR IGNORE INTO blocks (user_id, blocked_id) VALUES (?1, ?2)",
            params![me, other],
        )?;
        ctx.db.execute(
            "DELETE FROM friends WHERE (from_id = ?1 AND to_id = ?2) OR (from_id = ?2 AND to_id = ?1)",
            params![me, other],
        )?;
    } else {
        ctx.db.execute(
            "DELETE FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
            params![me, other],
        )?;
    }
    Ok(Res::empty())
}
