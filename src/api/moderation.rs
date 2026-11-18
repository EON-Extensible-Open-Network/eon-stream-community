// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Reports, the moderation queue, and appeals (madde 12).
//!
//! Every decision is made by a person, logged with its note, told to the
//! people it concerns, and can be appealed once -- to a different
//! moderator. Nothing here decides on its own.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::number;
use crate::http::{ApiError, Out, Req, Res};
use crate::{notify, paged, title_json, user_id, username, Ctx, User, PAGE};

fn log(
    ctx: &Ctx,
    moderator: &User,
    action: &str,
    item: &Value,
    subject: Option<i64>,
    note: Option<&str>,
) -> Result<i64, ApiError> {
    let subject_name = match subject {
        Some(id) => Some(username(ctx.db, id)?),
        None => None,
    };
    ctx.db.execute(
        "INSERT INTO mod_log (moderator, action, item, subject_user, note, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![moderator.username, action, item.to_string(), subject_name, note, ctx.now],
    )?;
    let entry = ctx.db.last_insert_rowid();
    if let Some(id) = subject {
        notify(
            ctx.db,
            id,
            "decision",
            &json!({ "log": entry, "action": action, "item": item, "note": note }),
            ctx.now,
        )?;
    }
    Ok(entry)
}

pub fn report(ctx: &Ctx, req: &Req) -> Out {
    let kind = req.text("kind").unwrap_or_default();
    if !matches!(kind.as_str(), "review" | "fact" | "list" | "poll" | "user") {
        return Err(ApiError::invalid("unknown kind of report"));
    }
    let target = req.body.get("target").cloned().unwrap_or(Value::Null);
    if !target.is_object() {
        return Err(ApiError::invalid("a report names what it is about"));
    }
    let reason = req.text("reason").unwrap_or_default();
    if !(3..=2000).contains(&reason.chars().count()) {
        return Err(ApiError::invalid("say why, in up to 2000 characters"));
    }
    ctx.db.execute(
        "INSERT INTO reports (reporter_id, kind, target, reason, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ctx.uid(), kind, target.to_string(), reason, ctx.now],
    )?;
    Ok(Res::accepted(json!({ "id": ctx.db.last_insert_rowid() })))
}

/// What a report points at, in words a moderator can judge.
fn reported(ctx: &Ctx, kind: &str, target: &Value) -> Result<Value, ApiError> {
    let text = |key: &str| {
        target
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Ok(match kind {
        "review" => {
            let author = user_id(ctx.db, &text("username")).ok();
            let review: Option<(i64, Option<String>)> = match author {
                Some(a) => ctx
                    .db
                    .query_row(
                        "SELECT score, review FROM ratings WHERE user_id = ?1 AND type = ?2 AND id = ?3",
                        params![a, text("type"), text("id")],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?,
                None => None,
            };
            json!({ "review": review.map(|(s, r)| json!({ "score": s, "review": r })), "title": title_json(ctx.db, &text("type"), &text("id"))? })
        }
        "fact" => {
            let id = target.get("id").and_then(Value::as_i64).unwrap_or(-1);
            json!({ "fact": ctx.db.query_row("SELECT kind, text FROM facts WHERE id = ?1", params![id], |r| Ok(json!({ "kind": r.get::<_, String>(0)?, "text": r.get::<_, String>(1)? }))).optional()? })
        }
        "list" => {
            let id = target.get("id").and_then(Value::as_i64).unwrap_or(-1);
            json!({ "list": ctx.db.query_row("SELECT name, description FROM lists WHERE id = ?1", params![id], |r| Ok(json!({ "name": r.get::<_, String>(0)?, "description": r.get::<_, Option<String>>(1)? }))).optional()? })
        }
        "poll" => {
            let id = target.get("id").and_then(Value::as_i64).unwrap_or(-1);
            json!({ "poll": ctx.db.query_row("SELECT question FROM polls WHERE id = ?1", params![id], |r| r.get::<_, String>(0)).optional()? })
        }
        _ => {
            json!({ "bio": ctx.db.query_row("SELECT bio FROM users WHERE username = ?1", params![text("username")], |r| r.get::<_, Option<String>>(0)).optional()?.flatten() })
        }
    })
}

pub fn queue(ctx: &Ctx, req: &Req) -> Out {
    let me = ctx.moderator()?;
    let want = req.q("kind");
    let take = |kind: &str| want.is_none_or(|w| w == kind);
    let mut items: Vec<Value> = Vec::new();
    if take("fact") {
        let mut s = ctx.db.prepare("SELECT id, subject_kind, subject, kind, text, spoiler, author_id, at FROM facts WHERE state = 'pending' ORDER BY at")?;
        for row in s.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, i64>(7)?,
            ))
        })? {
            let (id, subject_kind, subject, kind, text, spoiler, author, at) = row?;
            let about = if subject_kind == "title" {
                let (k, i) = subject.split_once(':').unwrap_or(("", ""));
                title_json(ctx.db, k, i)?
            } else {
                json!({ "person": subject })
            };
            items.push(json!({ "item": format!("fact-{id}"), "kind": "fact", "at": at, "author": author.map(|a| username(ctx.db, a)).transpose()?,
                "content": { "subject_kind": subject_kind, "subject": subject, "about": about, "fact_kind": kind, "text": text, "spoiler": spoiler } }));
        }
    }
    if take("edit") {
        let mut s = ctx.db.prepare("SELECT id, person_id, name, field, value, author_id, at FROM person_edits WHERE state = 'pending' ORDER BY at")?;
        for row in s.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })? {
            let (id, person, name, field, value, author, at) = row?;
            items.push(json!({ "item": format!("edit-{id}"), "kind": "edit", "at": at, "author": author.map(|a| username(ctx.db, a)).transpose()?,
                "content": { "person": person, "name": name, "field": field, "value": value } }));
        }
    }
    if take("nomination") {
        let mut s = ctx.db.prepare("SELECT id, event_id, year, category, type, title_id, person, won, author_id, at FROM nominations WHERE state = 'pending' ORDER BY at")?;
        for row in s.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })? {
            let (id, event, year, category, kind, title_id, person, won, author, at) = row?;
            let title = match (&kind, &title_id) {
                (Some(k), Some(i)) => title_json(ctx.db, k, i)?,
                _ => Value::Null,
            };
            items.push(json!({ "item": format!("nomination-{id}"), "kind": "nomination", "at": at, "author": author.map(|a| username(ctx.db, a)).transpose()?,
                "content": { "event": event, "year": year, "category": category, "title": title, "person": person, "won": won } }));
        }
    }
    if take("poll") {
        let mut s = ctx.db.prepare(
            "SELECT id, question, author_id, at FROM polls WHERE state = 'pending' ORDER BY at",
        )?;
        for row in s.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })? {
            let (id, question, author, at) = row?;
            let mut o = ctx.db.prepare(
                "SELECT label, type, title_id FROM poll_options WHERE poll_id = ?1 ORDER BY idx",
            )?;
            let options = o.query_map(params![id], |r| Ok(json!({ "label": r.get::<_, Option<String>>(0)?, "type": r.get::<_, Option<String>>(1)?, "id": r.get::<_, Option<String>>(2)? })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            items.push(json!({ "item": format!("poll-{id}"), "kind": "poll", "at": at, "author": author.map(|a| username(ctx.db, a)).transpose()?,
                "content": { "question": question, "options": options } }));
        }
    }
    if take("report") {
        let mut s = ctx.db.prepare(
            "SELECT id, kind, target, reason, at FROM reports WHERE state = 'open' ORDER BY at",
        )?;
        for row in s.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })? {
            let (id, kind, target, reason, at) = row?;
            let target: Value = serde_json::from_str(&target).unwrap_or(Value::Null);
            items.push(json!({ "item": format!("report-{id}"), "kind": "report", "at": at,
                "content": { "report_kind": kind, "target": target, "reason": reason, "reported": reported(ctx, &kind, &target)? } }));
        }
    }
    if take("appeal") {
        // An appeal goes to someone other than the moderator who decided.
        let mut s = ctx.db.prepare(
            "SELECT a.id, a.text, a.at, l.id, l.action, l.item, l.note, l.moderator, u.username FROM appeals a
             JOIN mod_log l ON l.id = a.log_id JOIN users u ON u.id = a.user_id WHERE a.state = 'open' AND l.moderator != ?1 ORDER BY a.at",
        )?;
        for row in s.query_map(params![me.username], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
            ))
        })? {
            let (id, text, at, log_id, action, item, note, moderator, user) = row?;
            items.push(json!({ "item": format!("appeal-{id}"), "kind": "appeal", "at": at, "author": user,
                "content": { "text": text, "decision": { "log": log_id, "action": action, "item": serde_json::from_str::<Value>(&item).unwrap_or(Value::Null), "note": note, "moderator": moderator } } }));
        }
    }
    items.sort_by(|a, b| a["at"].as_i64().cmp(&b["at"].as_i64()));
    let start = (req.page() * PAGE) as usize;
    let items: Vec<Value> = items
        .into_iter()
        .skip(start)
        .take(PAGE as usize + 1)
        .collect();
    let (items, more) = paged(items, PAGE);
    Ok(Res::ok(json!({ "items": items, "more": more })))
}

fn author_of(ctx: &Ctx, table: &str, id: i64) -> Result<Option<i64>, ApiError> {
    Ok(ctx
        .db
        .query_row(
            &format!("SELECT author_id FROM {table} WHERE id = ?1"),
            params![id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Take down what a report named. Answers who it belonged to.
fn take_down(ctx: &Ctx, kind: &str, target: &Value, undo: bool) -> Result<Option<i64>, ApiError> {
    let text = |key: &str| {
        target
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let id = target.get("id").and_then(Value::as_i64).unwrap_or(-1);
    Ok(match kind {
        "review" => {
            let author = user_id(ctx.db, &text("username")).ok();
            if let Some(a) = author {
                ctx.db.execute(
                    "UPDATE ratings SET hidden = ?1 WHERE user_id = ?2 AND type = ?3 AND id = ?4",
                    params![!undo, a, text("type"), text("id")],
                )?;
            }
            author
        }
        "fact" => {
            ctx.db.execute(
                "UPDATE facts SET state = ?1 WHERE id = ?2",
                params![if undo { "approved" } else { "removed" }, id],
            )?;
            author_of(ctx, "facts", id)?
        }
        "list" => {
            ctx.db.execute(
                "UPDATE lists SET hidden = ?1 WHERE id = ?2",
                params![!undo, id],
            )?;
            ctx.db
                .query_row(
                    "SELECT owner_id FROM lists WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?
        }
        "poll" => {
            ctx.db.execute(
                "UPDATE polls SET state = ?1 WHERE id = ?2",
                params![if undo { "approved" } else { "removed" }, id],
            )?;
            author_of(ctx, "polls", id)?
        }
        _ => {
            let user = user_id(ctx.db, &text("username")).ok();
            if let Some(u) = user {
                let until = if undo {
                    None
                } else {
                    Some(ctx.now + 30 * 86_400)
                };
                ctx.db.execute(
                    "UPDATE users SET suspended_until = ?1 WHERE id = ?2",
                    params![until, u],
                )?;
            }
            user
        }
    })
}

pub fn decide(ctx: &Ctx, req: &Req, item: &str) -> Out {
    let me = ctx.moderator()?.clone();
    let decision = req.text("decision").unwrap_or_default();
    let note = req.text("note");
    if note.as_ref().is_some_and(|n| n.chars().count() > 2000) {
        return Err(ApiError::invalid("a note is up to 2000 characters"));
    }
    let (kind, id) = item.split_once('-').ok_or_else(ApiError::not_found)?;
    let id = number(id)?;
    let approve = match decision.as_str() {
        "approve" => true,
        "reject" | "remove" | "dismiss" => false,
        _ => {
            return Err(ApiError::invalid(
                "decide approve, reject, remove or dismiss",
            ))
        }
    };
    let state = if approve { "approved" } else { "rejected" };
    let pending = |table: &str| -> Result<(), ApiError> {
        let n = ctx.db.execute(
            &format!("UPDATE {table} SET state = ?1 WHERE id = ?2 AND state = 'pending'"),
            params![state, id],
        )?;
        if n == 0 {
            Err(ApiError::not_found())
        } else {
            Ok(())
        }
    };
    let entry = match kind {
        "fact" => {
            pending("facts")?;
            log(
                ctx,
                &me,
                &decision,
                &json!({ "fact": id }),
                author_of(ctx, "facts", id)?,
                note.as_deref(),
            )?
        }
        "nomination" => {
            pending("nominations")?;
            log(
                ctx,
                &me,
                &decision,
                &json!({ "nomination": id }),
                author_of(ctx, "nominations", id)?,
                note.as_deref(),
            )?
        }
        "poll" => {
            pending("polls")?;
            log(
                ctx,
                &me,
                &decision,
                &json!({ "poll": id }),
                author_of(ctx, "polls", id)?,
                note.as_deref(),
            )?
        }
        "edit" => {
            let edit: (String, String, String, Option<String>) = ctx
                .db
                .query_row("SELECT person_id, name, field, value FROM person_edits WHERE id = ?1 AND state = 'pending'", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .optional()?
                .ok_or_else(ApiError::not_found)?;
            pending("person_edits")?;
            if approve {
                let (person, name, field, value) = edit;
                ctx.db.execute(
                    "INSERT INTO people (id, name) VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING",
                    params![person, name],
                )?;
                let column = match field.as_str() {
                    "born" => "born",
                    "died" => "died",
                    "birthplace" => "birthplace",
                    _ => "bio",
                };
                ctx.db.execute(
                    &format!("UPDATE people SET {column} = ?1 WHERE id = ?2"),
                    params![value, person],
                )?;
            }
            log(
                ctx,
                &me,
                &decision,
                &json!({ "edit": id }),
                author_of(ctx, "person_edits", id)?,
                note.as_deref(),
            )?
        }
        "report" => {
            let (report_kind, target, reporter): (String, String, Option<i64>) = ctx
                .db
                .query_row("SELECT kind, target, reporter_id FROM reports WHERE id = ?1 AND state = 'open'", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?
                .ok_or_else(ApiError::not_found)?;
            let target: Value = serde_json::from_str(&target).unwrap_or(Value::Null);
            let upheld = decision == "remove";
            ctx.db.execute(
                "UPDATE reports SET state = ?1 WHERE id = ?2",
                params![if upheld { "upheld" } else { "dismissed" }, id],
            )?;
            let owner = if upheld {
                take_down(ctx, &report_kind, &target, false)?
            } else {
                None
            };
            let entry = log(
                ctx,
                &me,
                &decision,
                &json!({ "report": id, "kind": report_kind, "target": target }),
                owner,
                note.as_deref(),
            )?;
            // The person who reported is told what came of it.
            if let Some(reporter) = reporter {
                notify(
                    ctx.db,
                    reporter,
                    "decision",
                    &json!({ "report": id, "outcome": if upheld { "upheld" } else { "dismissed" }, "note": note }),
                    ctx.now,
                )?;
            }
            entry
        }
        "appeal" => {
            let (log_id, user, decided_by): (i64, i64, String) = ctx
                .db
                .query_row(
                    "SELECT a.log_id, a.user_id, l.moderator FROM appeals a JOIN mod_log l ON l.id = a.log_id WHERE a.id = ?1 AND a.state = 'open'",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(ApiError::not_found)?;
            if decided_by == me.username {
                return Err(ApiError::forbidden(
                    "an appeal is decided by a different moderator",
                ));
            }
            ctx.db.execute(
                "UPDATE appeals SET state = ?1 WHERE id = ?2",
                params![if approve { "upheld" } else { "rejected" }, id],
            )?;
            if approve {
                // Undo what was decided.
                let item: String = ctx.db.query_row(
                    "SELECT item FROM mod_log WHERE id = ?1",
                    params![log_id],
                    |r| r.get(0),
                )?;
                let item: Value = serde_json::from_str(&item).unwrap_or(Value::Null);
                if let (Some(kind), Some(target)) =
                    (item.get("kind").and_then(Value::as_str), item.get("target"))
                {
                    take_down(ctx, kind, target, true)?;
                } else if let Some(fact) = item.get("fact").and_then(Value::as_i64) {
                    ctx.db.execute(
                        "UPDATE facts SET state = 'approved' WHERE id = ?1",
                        params![fact],
                    )?;
                } else if let Some(poll) = item.get("poll").and_then(Value::as_i64) {
                    ctx.db.execute(
                        "UPDATE polls SET state = 'approved' WHERE id = ?1",
                        params![poll],
                    )?;
                } else if let Some(nomination) = item.get("nomination").and_then(Value::as_i64) {
                    ctx.db.execute(
                        "UPDATE nominations SET state = 'approved' WHERE id = ?1",
                        params![nomination],
                    )?;
                } else if item.get("suspend").is_some() {
                    ctx.db.execute(
                        "UPDATE users SET suspended_until = NULL WHERE id = ?1",
                        params![user],
                    )?;
                }
            }
            log(
                ctx,
                &me,
                if approve {
                    "appeal_upheld"
                } else {
                    "appeal_rejected"
                },
                &json!({ "appeal": id, "decision": log_id }),
                Some(user),
                note.as_deref(),
            )?
        }
        _ => return Err(ApiError::not_found()),
    };
    Ok(Res::ok(
        json!({ "item": item, "decision": decision, "log": entry }),
    ))
}

pub fn suspend(ctx: &Ctx, req: &Req, name: &str) -> Out {
    let me = ctx.moderator()?.clone();
    let user = user_id(ctx.db, name)?;
    let until = req.body.get("suspended_until").and_then(Value::as_i64);
    ctx.db.execute(
        "UPDATE users SET suspended_until = ?1 WHERE id = ?2",
        params![until, user],
    )?;
    let entry = log(
        ctx,
        &me,
        if until.is_some() {
            "suspend"
        } else {
            "unsuspend"
        },
        &json!({ "suspend": name, "until": until }),
        Some(user),
        req.text("note").as_deref(),
    )?;
    Ok(Res::ok(
        json!({ "username": name, "suspended_until": until, "log": entry }),
    ))
}

pub fn role(ctx: &Ctx, req: &Req, name: &str) -> Out {
    let me = ctx.admin()?.clone();
    let role = req.text("role").unwrap_or_default();
    if !matches!(role.as_str(), "user" | "moderator" | "admin") {
        return Err(ApiError::invalid("a role is user, moderator or admin"));
    }
    let user = user_id(ctx.db, name)?;
    if user == me.id && role != "admin" {
        let admins: i64 =
            ctx.db
                .query_row("SELECT COUNT(*) FROM users WHERE role = 'admin'", [], |r| {
                    r.get(0)
                })?;
        if admins <= 1 {
            return Err(ApiError::forbidden(
                "the last administrator cannot step down",
            ));
        }
    }
    ctx.db.execute(
        "UPDATE users SET role = ?1 WHERE id = ?2",
        params![role, user],
    )?;
    let entry = log(
        ctx,
        &me,
        "role",
        &json!({ "role": role, "username": name }),
        Some(user),
        None,
    )?;
    Ok(Res::ok(
        json!({ "username": name, "role": role, "log": entry }),
    ))
}

pub fn log_page(ctx: &Ctx, req: &Req) -> Out {
    ctx.moderator()?;
    let mut s = ctx.db.prepare("SELECT id, moderator, action, item, subject_user, note, at, appealed FROM mod_log ORDER BY id DESC LIMIT ?1 OFFSET ?2")?;
    let entries = s
        .query_map(params![PAGE + 1, req.page() * PAGE], |r| {
            let item: String = r.get(3)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "moderator": r.get::<_, String>(1)?, "action": r.get::<_, String>(2)?,
                "item": serde_json::from_str::<Value>(&item).unwrap_or(Value::Null), "subject": r.get::<_, Option<String>>(4)?,
                "note": r.get::<_, Option<String>>(5)?, "at": r.get::<_, i64>(6)?, "appealed": r.get::<_, bool>(7)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (entries, more) = paged(entries, PAGE);
    Ok(Res::ok(json!({ "entries": entries, "more": more })))
}

pub fn appeal(ctx: &Ctx, req: &Req) -> Out {
    let user = ctx.me()?;
    let decision = req
        .body
        .get("decision")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::invalid("say which decision"))?;
    let text = req.text("text").unwrap_or_default();
    if !(10..=2000).contains(&text.chars().count()) {
        return Err(ApiError::invalid("an appeal is 10 to 2000 characters"));
    }
    let entry: Option<(Option<String>, bool)> = ctx
        .db
        .query_row(
            "SELECT subject_user, appealed FROM mod_log WHERE id = ?1",
            params![decision],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((subject, appealed)) = entry else {
        return Err(ApiError::not_found());
    };
    if subject.as_deref() != Some(user.username.as_str()) {
        return Err(ApiError::forbidden(
            "only the person a decision concerns can appeal it",
        ));
    }
    if appealed {
        return Err(ApiError::taken("this decision was already appealed"));
    }
    ctx.db.execute(
        "UPDATE mod_log SET appealed = 1 WHERE id = ?1",
        params![decision],
    )?;
    ctx.db.execute(
        "INSERT INTO appeals (user_id, log_id, text, at) VALUES (?1, ?2, ?3, ?4)",
        params![user.id, decision, text, ctx.now],
    )?;
    Ok(Res::accepted(json!({ "id": ctx.db.last_insert_rowid() })))
}
