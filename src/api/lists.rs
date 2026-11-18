// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Lists, and the one watchlist every account has.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::number;
use crate::http::{ApiError, Out, Req, Res};
use crate::{
    check_id, check_type, notify, paged, remember_title, title_json, user_id, username, Ctx, PAGE,
};

const MAX_ITEMS: i64 = 5000;
const ITEMS_PAGE: i64 = 50;

struct List {
    id: i64,
    owner: i64,
    public: bool,
    kind: String,
}

fn load(ctx: &Ctx, id: i64) -> Result<List, ApiError> {
    let list: Option<List> = ctx
        .db
        .query_row(
            "SELECT id, owner_id, public, kind, hidden FROM lists WHERE id = ?1",
            params![id],
            |r| {
                let hidden: bool = r.get(4)?;
                Ok((
                    List {
                        id: r.get(0)?,
                        owner: r.get(1)?,
                        public: r.get(2)?,
                        kind: r.get(3)?,
                    },
                    hidden,
                ))
            },
        )
        .optional()?
        .and_then(|(list, hidden)| (!hidden || ctx.uid() == Some(list.owner)).then_some(list));
    let list = list.ok_or_else(ApiError::not_found)?;
    if !list.public && ctx.uid() != Some(list.owner) {
        return Err(ApiError::not_found());
    }
    Ok(list)
}

fn own(ctx: &Ctx, id: &str) -> Result<List, ApiError> {
    let user = ctx.writer()?;
    let list = load(ctx, number(id)?)?;
    if list.owner != user.id {
        return Err(ApiError::forbidden("only its owner changes a list"));
    }
    Ok(list)
}

fn summary(ctx: &Ctx, id: i64) -> Result<Value, ApiError> {
    let me = ctx.uid().unwrap_or(-1);
    Ok(ctx.db.query_row(
        "SELECT l.id, l.owner_id, l.name, l.description, l.public, l.ranked, l.kind, l.updated_at,
           (SELECT COUNT(*) FROM list_likes k WHERE k.list_id = l.id),
           EXISTS(SELECT 1 FROM list_likes k WHERE k.list_id = l.id AND k.user_id = ?2),
           (SELECT COUNT(*) FROM list_items i WHERE i.list_id = l.id)
         FROM lists l WHERE l.id = ?1",
        params![id, me],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "owner": username(ctx.db, r.get(1)?)?,
                "name": r.get::<_, String>(2)?, "description": r.get::<_, Option<String>>(3)?,
                "public": r.get::<_, bool>(4)?, "ranked": r.get::<_, bool>(5)?, "kind": r.get::<_, String>(6)?,
                "updated_at": r.get::<_, i64>(7)?, "likes": r.get::<_, i64>(8)?, "liked": r.get::<_, bool>(9)?,
                "count": r.get::<_, i64>(10)?,
            }))
        },
    )?)
}

fn with_items(ctx: &Ctx, list: &List, page: i64) -> Out {
    let mut value = summary(ctx, list.id)?;
    let mut statement = ctx.db.prepare(
        "SELECT type, id, note, position, at FROM list_items WHERE list_id = ?1 ORDER BY position, at LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement
        .query_map(params![list.id, ITEMS_PAGE + 1, page * ITEMS_PAGE], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (kind, id, note, position, at) in rows {
        items.push(json!({ "type": kind, "id": id, "title": title_json(ctx.db, &kind, &id)?, "note": note, "position": position, "at": at }));
    }
    let (items, more) = paged(items, ITEMS_PAGE);
    value["items"] = json!(items);
    value["more"] = json!(more);
    Ok(Res::ok(value))
}

fn fields(req: &Req) -> Result<(String, Option<String>, bool, bool), ApiError> {
    let name = req.text("name").unwrap_or_default();
    if !(1..=100).contains(&name.chars().count()) {
        return Err(ApiError::invalid("a list's name is 1 to 100 characters"));
    }
    let description = req.text("description");
    if description
        .as_ref()
        .is_some_and(|d| d.chars().count() > 1000)
    {
        return Err(ApiError::invalid("a description is up to 1000 characters"));
    }
    let public = req
        .body
        .get("public")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok((name, description, public, req.flag("ranked")))
}

pub fn find(ctx: &Ctx, req: &Req) -> Out {
    let me = ctx.uid().unwrap_or(-1);
    let owner = match req.q("owner") {
        Some(name) => Some(user_id(ctx.db, name)?),
        None => None,
    };
    let order = if req.q("sort") == Some("popular") {
        "(SELECT COUNT(*) FROM list_likes k WHERE k.list_id = l.id) DESC, l.updated_at DESC"
    } else {
        "l.updated_at DESC"
    };
    let sql = format!(
        "SELECT l.id FROM lists l WHERE (?1 IS NULL OR l.owner_id = ?1) AND (l.public = 1 OR l.owner_id = ?2)
           AND (l.hidden = 0 OR l.owner_id = ?2) AND (l.kind = 'custom' OR l.owner_id = ?2 OR l.public = 1)
         ORDER BY {order} LIMIT ?3 OFFSET ?4"
    );
    let mut statement = ctx.db.prepare(&sql)?;
    let ids: Vec<i64> = statement
        .query_map(params![owner, me, PAGE + 1, req.page() * PAGE], |r| {
            r.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut lists = Vec::new();
    for id in ids {
        lists.push(summary(ctx, id)?);
    }
    let (lists, more) = paged(lists, PAGE);
    Ok(Res::ok(json!({ "lists": lists, "more": more })))
}

pub fn create(ctx: &Ctx, req: &Req) -> Out {
    let user = ctx.writer()?;
    let (name, description, public, ranked) = fields(req)?;
    let count: i64 = ctx.db.query_row(
        "SELECT COUNT(*) FROM lists WHERE owner_id = ?1",
        params![user.id],
        |r| r.get(0),
    )?;
    if count >= 500 {
        return Err(ApiError::invalid("an account has up to 500 lists"));
    }
    ctx.db.execute(
        "INSERT INTO lists (owner_id, name, description, public, ranked, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![user.id, name, description, public, ranked, ctx.now],
    )?;
    let id = ctx.db.last_insert_rowid();
    let mut res = with_items(ctx, &load(ctx, id)?, 0)?;
    res.status = 201;
    Ok(res)
}

pub fn get(ctx: &Ctx, req: &Req, list: &str) -> Out {
    with_items(ctx, &load(ctx, number(list)?)?, req.page())
}

pub fn update(ctx: &Ctx, req: &Req, list: &str) -> Out {
    let found = own(ctx, list)?;
    let (name, description, public, ranked) = fields(req)?;
    ctx.db.execute(
        "UPDATE lists SET name = ?1, description = ?2, public = ?3, ranked = ?4, updated_at = ?5 WHERE id = ?6",
        params![name, description, public, ranked, ctx.now, found.id],
    )?;
    with_items(ctx, &load(ctx, found.id)?, 0)
}

pub fn delete(ctx: &Ctx, list: &str) -> Out {
    let found = own(ctx, list)?;
    if found.kind == "watchlist" {
        return Err(ApiError::forbidden("the watchlist stays; empty it instead"));
    }
    ctx.db
        .execute("DELETE FROM lists WHERE id = ?1", params![found.id])?;
    Ok(Res::empty())
}

pub fn put_item(ctx: &Ctx, req: &Req, list: &str, kind: &str, id: &str) -> Out {
    let found = own(ctx, list)?;
    check_type(kind)?;
    check_id(id)?;
    let note = req.text("note");
    if note.as_ref().is_some_and(|n| n.chars().count() > 500) {
        return Err(ApiError::invalid("a note is up to 500 characters"));
    }
    let count: i64 = ctx.db.query_row(
        "SELECT COUNT(*) FROM list_items WHERE list_id = ?1",
        params![found.id],
        |r| r.get(0),
    )?;
    let exists: bool = ctx.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM list_items WHERE list_id = ?1 AND type = ?2 AND id = ?3)",
        params![found.id, kind, id],
        |r| r.get(0),
    )?;
    if !exists && count >= MAX_ITEMS {
        return Err(ApiError::invalid("a list holds up to 5000 titles"));
    }
    remember_title(
        ctx.db,
        kind,
        id,
        req.body.get("title").unwrap_or(&Value::Null),
    )?;
    let position = req
        .body
        .get("position")
        .and_then(Value::as_i64)
        .unwrap_or(count + 1);
    ctx.db.execute(
        "INSERT INTO list_items (list_id, type, id, note, position, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(list_id, type, id) DO UPDATE SET note = excluded.note, position = excluded.position",
        params![found.id, kind, id, note, position, ctx.now],
    )?;
    ctx.db.execute(
        "UPDATE lists SET updated_at = ?1 WHERE id = ?2",
        params![ctx.now, found.id],
    )?;
    Ok(Res::ok(
        json!({ "type": kind, "id": id, "title": title_json(ctx.db, kind, id)?, "note": note, "position": position, "at": ctx.now }),
    ))
}

pub fn delete_item(ctx: &Ctx, list: &str, kind: &str, id: &str) -> Out {
    let found = own(ctx, list)?;
    ctx.db.execute(
        "DELETE FROM list_items WHERE list_id = ?1 AND type = ?2 AND id = ?3",
        params![found.id, kind, id],
    )?;
    ctx.db.execute(
        "UPDATE lists SET updated_at = ?1 WHERE id = ?2",
        params![ctx.now, found.id],
    )?;
    Ok(Res::empty())
}

pub fn like(ctx: &Ctx, list: &str, set: bool) -> Out {
    let user = ctx.writer()?;
    let found = load(ctx, number(list)?)?;
    if !found.public {
        return Err(ApiError::not_found());
    }
    if set {
        let added = ctx.db.execute(
            "INSERT OR IGNORE INTO list_likes (list_id, user_id) VALUES (?1, ?2)",
            params![found.id, user.id],
        )?;
        if added > 0 && found.owner != user.id {
            let name: String = ctx.db.query_row(
                "SELECT name FROM lists WHERE id = ?1",
                params![found.id],
                |r| r.get(0),
            )?;
            notify(
                ctx.db,
                found.owner,
                "list_like",
                &json!({ "username": user.username, "list": found.id, "name": name }),
                ctx.now,
            )?;
        }
    } else {
        ctx.db.execute(
            "DELETE FROM list_likes WHERE list_id = ?1 AND user_id = ?2",
            params![found.id, user.id],
        )?;
    }
    let likes: i64 = ctx.db.query_row(
        "SELECT COUNT(*) FROM list_likes WHERE list_id = ?1",
        params![found.id],
        |r| r.get(0),
    )?;
    Ok(Res::ok(json!({ "likes": likes })))
}

pub fn of_title(ctx: &Ctx, req: &Req, kind: &str, id: &str) -> Out {
    check_type(kind)?;
    check_id(id)?;
    let mut statement = ctx.db.prepare(
        "SELECT l.id FROM lists l JOIN list_items i ON i.list_id = l.id
         WHERE i.type = ?1 AND i.id = ?2 AND l.public = 1 AND l.hidden = 0
         ORDER BY (SELECT COUNT(*) FROM list_likes k WHERE k.list_id = l.id) DESC LIMIT ?3 OFFSET ?4",
    )?;
    let ids: Vec<i64> = statement
        .query_map(params![kind, id, PAGE + 1, req.page() * PAGE], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut lists = Vec::new();
    for list in ids {
        lists.push(summary(ctx, list)?);
    }
    let (lists, more) = paged(lists, PAGE);
    Ok(Res::ok(json!({ "lists": lists, "more": more })))
}

pub fn watchlist(ctx: &Ctx, req: &Req) -> Out {
    let user = ctx.me()?;
    let id: i64 = ctx.db.query_row(
        "SELECT id FROM lists WHERE owner_id = ?1 AND kind = 'watchlist'",
        params![user.id],
        |r| r.get(0),
    )?;
    with_items(ctx, &load(ctx, id)?, req.page())
}
