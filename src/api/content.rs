// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! What people add to the database: facts about titles and people, the
//! parental guide, skip markers, people's details, awards and polls.
//! Everything with text goes through the moderation queue first.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use super::number;
use crate::http::{ApiError, Out, Req, Res};
use crate::{
    check_id, check_type, paged, person_id, remember_title, title_json, username, Ctx, PAGE,
};

const TITLE_FACTS: &[&str] = &[
    "trivia",
    "goof",
    "quote",
    "crazy_credit",
    "alternate_version",
    "soundtrack",
    "connection",
    "keyword",
    "tagline",
    "synopsis",
    "plot",
    "location",
    "release",
    "technical",
    "box_office",
    "faq",
];
const PERSON_FACTS: &[&str] = &["trivia", "quote", "bio", "nickname", "trademark", "salary"];
const CATEGORIES: &[&str] = &["sex", "violence", "profanity", "substances", "frightening"];
const LEVELS: &[&str] = &["none", "mild", "moderate", "severe"];
const SEGMENTS: &[&str] = &["intro", "recap", "credits", "preview"];

// --------------------------------------------------------------------- facts

fn fact_json(ctx: &Ctx, row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: i64 = row.get(0)?;
    let author: Option<i64> = row.get(5)?;
    let (up, down, mine): (i64, i64, Option<bool>) = ctx.db.query_row(
        "SELECT COALESCE(SUM(interesting = 1), 0), COALESCE(SUM(interesting = 0), 0),
           (SELECT interesting FROM fact_votes WHERE fact_id = ?1 AND user_id = ?2)
         FROM fact_votes WHERE fact_id = ?1",
        params![id, ctx.uid().unwrap_or(-1)],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(json!({
        "id": id, "kind": row.get::<_, String>(1)?, "text": row.get::<_, String>(2)?,
        "spoiler": row.get::<_, bool>(3)?, "state": row.get::<_, String>(4)?,
        "author": match author { Some(a) => Value::String(username(ctx.db, a)?), None => Value::Null },
        "at": row.get::<_, i64>(6)?,
        "interesting": up, "not_interesting": down,
        "my_vote": mine.map(|m| if m { "interesting" } else { "not_interesting" }),
    }))
}

pub fn facts(
    ctx: &Ctx,
    req: &Req,
    subject_kind: &str,
    subject: &str,
    title: Option<(&str, &str)>,
) -> Out {
    if let Some((kind, id)) = title {
        check_type(kind)?;
        check_id(id)?;
    }
    let me = ctx.uid().unwrap_or(-1);
    let kind = req.q("kind");
    let mut statement = ctx.db.prepare(
        "SELECT f.id, f.kind, f.text, f.spoiler, f.state, f.author_id, f.at,
           (SELECT COALESCE(SUM(CASE WHEN interesting = 1 THEN 1 ELSE -1 END), 0) FROM fact_votes v WHERE v.fact_id = f.id) AS score
         FROM facts f WHERE f.subject_kind = ?1 AND f.subject = ?2 AND (?3 IS NULL OR f.kind = ?3)
           AND (f.state = 'approved' OR (f.state = 'pending' AND f.author_id = ?4))
         ORDER BY score DESC, f.at DESC LIMIT ?5 OFFSET ?6",
    )?;
    let items = statement
        .query_map(
            params![subject_kind, subject, kind, me, PAGE + 1, req.page() * PAGE],
            |row| fact_json(ctx, row),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (items, more) = paged(items, PAGE);
    let mut counts = serde_json::Map::new();
    let mut statement = ctx.db.prepare(
        "SELECT kind, COUNT(*) FROM facts WHERE subject_kind = ?1 AND subject = ?2 AND state = 'approved' GROUP BY kind",
    )?;
    for row in statement.query_map(params![subject_kind, subject], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (k, n) = row?;
        counts.insert(k, json!(n));
    }
    Ok(Res::ok(
        json!({ "facts": items, "more": more, "counts": counts }),
    ))
}

pub fn add_fact(ctx: &Ctx, req: &Req, subject_kind: &str, kind: &str, id: &str) -> Out {
    let user = ctx.writer()?;
    let fact_kind = req.text("kind").unwrap_or_default();
    let allowed = if subject_kind == "title" {
        TITLE_FACTS
    } else {
        PERSON_FACTS
    };
    if !allowed.contains(&fact_kind.as_str()) {
        return Err(ApiError::invalid("unknown kind of fact"));
    }
    let text = req.text("text").unwrap_or_default();
    let length = text.chars().count();
    let (min, max) = if matches!(fact_kind.as_str(), "keyword" | "tagline" | "nickname") {
        (2, 200)
    } else {
        (10, 2000)
    };
    if length < min || length > max {
        return Err(ApiError::invalid(format!(
            "this kind of fact is {min} to {max} characters"
        )));
    }
    let subject = if subject_kind == "title" {
        check_type(kind)?;
        check_id(id)?;
        remember_title(
            ctx.db,
            kind,
            id,
            req.body.get("title").unwrap_or(&Value::Null),
        )?;
        format!("{kind}:{id}")
    } else {
        let pid = person_id(id);
        if pid.is_empty() || pid != id {
            return Err(ApiError::invalid("that is not a person id"));
        }
        let name = req.text("name").unwrap_or_else(|| id.replace('-', " "));
        ctx.db.execute(
            "INSERT INTO people (id, name) VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING",
            params![pid, name],
        )?;
        pid
    };
    ctx.db.execute(
        "INSERT INTO facts (subject_kind, subject, kind, text, spoiler, author_id, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![subject_kind, subject, fact_kind, text, req.flag("spoiler"), user.id, ctx.now],
    )?;
    Ok(Res::accepted(
        json!({ "id": ctx.db.last_insert_rowid(), "state": "pending" }),
    ))
}

pub fn vote_fact(ctx: &Ctx, req: &Req, fact: &str, set: bool) -> Out {
    let user = ctx.writer()?;
    let fact = number(fact)?;
    let author: Option<i64> = ctx
        .db
        .query_row(
            "SELECT author_id FROM facts WHERE id = ?1 AND state = 'approved'",
            params![fact],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(ApiError::not_found)?;
    if author == Some(user.id) {
        return Err(ApiError::forbidden("no one votes on what they wrote"));
    }
    if set {
        let value = req
            .body
            .get("interesting")
            .and_then(Value::as_bool)
            .ok_or_else(|| ApiError::invalid("say whether it was interesting"))?;
        ctx.db.execute(
            "INSERT INTO fact_votes (fact_id, user_id, interesting) VALUES (?1, ?2, ?3) ON CONFLICT DO UPDATE SET interesting = excluded.interesting",
            params![fact, user.id, value],
        )?;
    } else {
        ctx.db.execute(
            "DELETE FROM fact_votes WHERE fact_id = ?1 AND user_id = ?2",
            params![fact, user.id],
        )?;
    }
    let (up, down): (i64, i64) = ctx.db.query_row(
        "SELECT COALESCE(SUM(interesting = 1), 0), COALESCE(SUM(interesting = 0), 0) FROM fact_votes WHERE fact_id = ?1",
        params![fact],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(Res::ok(
        json!({ "interesting": up, "not_interesting": down }),
    ))
}

pub fn withdraw_fact(ctx: &Ctx, fact: &str) -> Out {
    let user = ctx.me()?;
    let removed = ctx.db.execute(
        "DELETE FROM facts WHERE id = ?1 AND author_id = ?2",
        params![number(fact)?, user.id],
    )?;
    if removed == 0 {
        return Err(ApiError::not_found());
    }
    Ok(Res::empty())
}

// ------------------------------------------------------------ parental guide

pub fn parental(ctx: &Ctx, kind: &str, id: &str) -> Out {
    check_type(kind)?;
    check_id(id)?;
    let me = ctx.uid().unwrap_or(-1);
    let mut categories = Vec::new();
    for category in CATEGORIES {
        let mut votes = serde_json::Map::new();
        let mut best = ("", 0_i64);
        let mut total = 0;
        for level in LEVELS {
            let n: i64 = ctx.db.query_row(
                "SELECT COUNT(*) FROM parental WHERE type = ?1 AND id = ?2 AND category = ?3 AND level = ?4",
                params![kind, id, category, level],
                |r| r.get(0),
            )?;
            total += n;
            if n > best.1 {
                best = (level, n);
            }
            votes.insert((*level).to_owned(), json!(n));
        }
        let mine: Option<String> = ctx
            .db
            .query_row(
                "SELECT level FROM parental WHERE user_id = ?1 AND type = ?2 AND id = ?3 AND category = ?4",
                params![me, kind, id, category],
                |r| r.get(0),
            )
            .optional()?;
        categories.push(json!({
            "category": category, "votes": votes,
            "verdict": if total >= 3 { Value::String(best.0.to_owned()) } else { Value::Null },
            "mine": mine,
        }));
    }
    Ok(Res::ok(json!({ "categories": categories })))
}

pub fn vote_parental(ctx: &Ctx, req: &Req, kind: &str, id: &str, category: &str) -> Out {
    let user = ctx.writer()?;
    check_type(kind)?;
    check_id(id)?;
    let level = req.text("level").unwrap_or_default();
    if !CATEGORIES.contains(&category) || !LEVELS.contains(&level.as_str()) {
        return Err(ApiError::invalid("unknown category or level"));
    }
    remember_title(
        ctx.db,
        kind,
        id,
        req.body.get("title").unwrap_or(&Value::Null),
    )?;
    ctx.db.execute(
        "INSERT INTO parental (user_id, type, id, category, level) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT DO UPDATE SET level = excluded.level",
        params![user.id, kind, id, category, level],
    )?;
    parental(ctx, kind, id)
}

// -------------------------------------------------------------- skip markers

fn segment_score(ctx: &Ctx, segment: i64) -> rusqlite::Result<i64> {
    ctx.db.query_row(
        "SELECT COALESCE(SUM(CASE WHEN up = 1 THEN 1 ELSE -1 END), 0) FROM segment_votes WHERE segment_id = ?1",
        params![segment],
        |r| r.get(0),
    )
}

pub fn segments(ctx: &Ctx, kind: &str, id: &str) -> Out {
    check_type(kind)?;
    check_id(id)?;
    let mut statement = ctx
        .db
        .prepare("SELECT id, kind, start, end FROM segments WHERE type = ?1 AND item = ?2")?;
    let mut best: std::collections::HashMap<String, (i64, Value)> =
        std::collections::HashMap::new();
    for row in statement.query_map(params![kind, id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })? {
        let (sid, k, start, end) = row?;
        let score = segment_score(ctx, sid)?;
        if score < -2 {
            continue;
        }
        if best.get(&k).is_none_or(|(s, _)| score > *s) {
            best.insert(
                k.clone(),
                (
                    score,
                    json!({ "id": sid, "kind": k, "start": start, "end": end, "score": score }),
                ),
            );
        }
    }
    let mut segments: Vec<Value> = best.into_values().map(|(_, v)| v).collect();
    segments.sort_by(|a, b| {
        a["start"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["start"].as_f64().unwrap_or(0.0))
    });
    Ok(Res::ok(json!({ "segments": segments })))
}

pub fn add_segment(ctx: &Ctx, req: &Req, kind: &str, id: &str) -> Out {
    let user = ctx.writer()?;
    check_type(kind)?;
    check_id(id)?;
    let segment_kind = req.text("kind").unwrap_or_default();
    let start = req
        .body
        .get("start")
        .and_then(Value::as_f64)
        .unwrap_or(-1.0);
    let end = req.body.get("end").and_then(Value::as_f64).unwrap_or(-1.0);
    let duration = req
        .body
        .get("duration")
        .and_then(Value::as_f64)
        .unwrap_or(f64::MAX);
    if !SEGMENTS.contains(&segment_kind.as_str())
        || start < 0.0
        || end <= start
        || end > duration
        || end - start > 900.0
    {
        return Err(ApiError::invalid(
            "a marker is a known kind, with a start before its end, inside the episode",
        ));
    }
    // Close to one already there: that is a vote for it.
    let near: Option<i64> = ctx
        .db
        .query_row(
            "SELECT id FROM segments WHERE type = ?1 AND item = ?2 AND kind = ?3 AND ABS(start - ?4) <= 2 AND ABS(end - ?5) <= 2",
            params![kind, id, segment_kind, start, end],
            |r| r.get(0),
        )
        .optional()?;
    let segment = if let Some(existing) = near {
        existing
    } else {
        ctx.db.execute(
            "INSERT INTO segments (type, item, kind, start, end, author_id, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![kind, id, segment_kind, start, end, user.id, ctx.now],
        )?;
        ctx.db.last_insert_rowid()
    };
    ctx.db.execute(
        "INSERT INTO segment_votes (segment_id, user_id, up) VALUES (?1, ?2, 1) ON CONFLICT DO UPDATE SET up = 1",
        params![segment, user.id],
    )?;
    Ok(Res::created(json!({ "id": segment })))
}

pub fn vote_segment(ctx: &Ctx, req: &Req, segment: &str) -> Out {
    let user = ctx.writer()?;
    let segment = number(segment)?;
    let exists: bool = ctx.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM segments WHERE id = ?1)",
        params![segment],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let up = req
        .body
        .get("up")
        .and_then(Value::as_bool)
        .ok_or_else(|| ApiError::invalid("say up or down"))?;
    ctx.db.execute(
        "INSERT INTO segment_votes (segment_id, user_id, up) VALUES (?1, ?2, ?3) ON CONFLICT DO UPDATE SET up = excluded.up",
        params![segment, user.id, up],
    )?;
    Ok(Res::ok(json!({ "score": segment_score(ctx, segment)? })))
}

// -------------------------------------------------------------------- people

type PersonRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn known_for(ctx: &Ctx, person: &str, limit: i64) -> rusqlite::Result<Vec<Value>> {
    let mut statement = ctx.db.prepare(
        "SELECT c.type, c.id, (SELECT COUNT(*) FROM ratings r WHERE r.type = c.type AND r.id = c.id) AS n
         FROM credits c WHERE c.person_id = ?1 GROUP BY c.type, c.id ORDER BY n DESC LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![person, limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(k, i)| Ok(json!({ "type": k, "id": i, "title": title_json(ctx.db, &k, &i)? })))
        .collect()
}

pub fn find_people(ctx: &Ctx, req: &Req) -> Out {
    let q = req.q("q").unwrap_or_default().trim().to_lowercase();
    if q.chars().count() < 2 {
        return Ok(Res::ok(json!({ "items": [] })));
    }
    let pattern = format!("{}%", person_id(&q));
    let mut statement = ctx.db.prepare(
        "SELECT id, name FROM people WHERE id LIKE ?1 OR name LIKE ?2 ORDER BY
           (SELECT COUNT(*) FROM credits c WHERE c.person_id = people.id) DESC LIMIT 20",
    )?;
    let rows = statement
        .query_map(params![pattern, format!("%{q}%")], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (id, name) in rows {
        items.push(json!({ "id": id, "name": name, "known_for": known_for(ctx, &id, 2)? }));
    }
    Ok(Res::ok(json!({ "items": items })))
}

pub fn person(ctx: &Ctx, person: &str) -> Out {
    let row: Option<PersonRow> = ctx
        .db
        .query_row(
            "SELECT name, born, died, birthplace, bio FROM people WHERE id = ?1",
            params![person],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((name, born, died, birthplace, bio)) = row else {
        return Err(ApiError::not_found());
    };
    let mut statement = ctx.db.prepare(
        "SELECT c.type, c.id, c.role, t.year FROM credits c LEFT JOIN titles t ON t.type = c.type AND t.id = c.id
         WHERE c.person_id = ?1 ORDER BY t.year DESC",
    )?;
    let rows = statement
        .query_map(params![person], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut filmography = Vec::new();
    for (k, i, role) in rows {
        filmography.push(
            json!({ "type": k, "id": i, "title": title_json(ctx.db, &k, &i)?, "role": role }),
        );
    }
    let mut statement = ctx.db.prepare(
        "SELECT n.event_id, e.name, n.year, n.category, n.type, n.title_id, n.won FROM nominations n
         JOIN award_events e ON e.id = n.event_id WHERE n.person_id = ?1 AND n.state = 'approved' ORDER BY n.year DESC",
    )?;
    let awards = statement
        .query_map(params![person], |r| {
            Ok(json!({
                "event": r.get::<_, String>(0)?, "event_name": r.get::<_, String>(1)?, "year": r.get::<_, i64>(2)?,
                "category": r.get::<_, String>(3)?, "type": r.get::<_, Option<String>>(4)?,
                "title_id": r.get::<_, Option<String>>(5)?, "won": r.get::<_, bool>(6)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Res::ok(json!({
        "id": person, "name": name, "born": born, "died": died, "birthplace": birthplace, "bio": bio,
        "known_for": known_for(ctx, person, 4)?, "filmography": filmography, "awards": awards,
    })))
}

pub fn edit_person(ctx: &Ctx, req: &Req, person: &str) -> Out {
    let user = ctx.writer()?;
    let field = req.text("field").unwrap_or_default();
    if !matches!(field.as_str(), "born" | "died" | "birthplace" | "bio") {
        return Err(ApiError::invalid("unknown field"));
    }
    let value = req.text("value");
    let max = if field == "bio" { 5000 } else { 120 };
    if value.as_ref().is_some_and(|v| v.chars().count() > max) {
        return Err(ApiError::invalid(format!(
            "that is longer than {max} characters"
        )));
    }
    if person_id(person) != person || person.is_empty() {
        return Err(ApiError::invalid("that is not a person id"));
    }
    let name = req.text("name").unwrap_or_else(|| person.replace('-', " "));
    ctx.db.execute(
        "INSERT INTO person_edits (person_id, name, field, value, author_id, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![person, name, field, value, user.id, ctx.now],
    )?;
    Ok(Res::accepted(
        json!({ "id": ctx.db.last_insert_rowid(), "state": "pending" }),
    ))
}

// -------------------------------------------------------------------- awards

pub fn award_events(ctx: &Ctx) -> Out {
    let mut statement = ctx
        .db
        .prepare("SELECT id, name FROM award_events ORDER BY name")?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut events = Vec::new();
    for (id, name) in rows {
        let mut years = ctx.db.prepare("SELECT DISTINCT year FROM nominations WHERE event_id = ?1 AND state = 'approved' ORDER BY year DESC")?;
        let years: Vec<i64> = years
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !years.is_empty() {
            events.push(json!({ "id": id, "name": name, "years": years }));
        }
    }
    Ok(Res::ok(json!({ "events": events })))
}

fn nomination_json(ctx: &Ctx, r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let kind: Option<String> = r.get(4)?;
    let title_id: Option<String> = r.get(5)?;
    let title = match (&kind, &title_id) {
        (Some(k), Some(i)) => title_json(ctx.db, k, i)?,
        _ => Value::Null,
    };
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "event": r.get::<_, String>(1)?, "year": r.get::<_, i64>(2)?,
        "category": r.get::<_, String>(3)?, "type": kind, "title_id": title_id, "title": title,
        "person": r.get::<_, Option<String>>(6)?, "person_id": r.get::<_, Option<String>>(7)?, "won": r.get::<_, bool>(8)?,
    }))
}

pub fn award_year(ctx: &Ctx, event: &str, year: &str) -> Out {
    let year = number(year)?;
    let name: String = ctx
        .db
        .query_row(
            "SELECT name FROM award_events WHERE id = ?1",
            params![event],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(ApiError::not_found)?;
    let mut statement = ctx.db.prepare(
        "SELECT id, event_id, year, category, type, title_id, person, person_id, won FROM nominations
         WHERE event_id = ?1 AND year = ?2 AND state = 'approved' ORDER BY category, won DESC",
    )?;
    let all = statement
        .query_map(params![event, year], |r| nomination_json(ctx, r))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut categories: Vec<(String, Vec<Value>)> = Vec::new();
    for nomination in all {
        let category = nomination["category"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        match categories.iter_mut().find(|(c, _)| *c == category) {
            Some((_, list)) => list.push(nomination),
            None => categories.push((category, vec![nomination])),
        }
    }
    let categories: Vec<Value> = categories
        .into_iter()
        .map(|(c, n)| json!({ "category": c, "nominations": n }))
        .collect();
    Ok(Res::ok(
        json!({ "event": { "id": event, "name": name }, "year": year, "categories": categories }),
    ))
}

pub fn nominate(ctx: &Ctx, req: &Req, event: &str, year: &str) -> Out {
    let user = ctx.writer()?;
    let year = number(year)?;
    if !(1900..=2100).contains(&year) || person_id(event) != event || event.is_empty() {
        return Err(ApiError::invalid("unknown event or year"));
    }
    let category = req
        .text("category")
        .filter(|c| c.chars().count() <= 200)
        .ok_or_else(|| ApiError::invalid("a nomination needs its category"))?;
    let event_name = req
        .text("event_name")
        .unwrap_or_else(|| event.replace('-', " "));
    ctx.db.execute(
        "INSERT INTO award_events (id, name) VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING",
        params![event, event_name],
    )?;
    let kind = req.text("type");
    let id = req.text("id");
    if let (Some(k), Some(i)) = (&kind, &id) {
        check_type(k)?;
        check_id(i)?;
        remember_title(ctx.db, k, i, req.body.get("title").unwrap_or(&Value::Null))?;
    }
    let person = req.text("person").filter(|p| p.chars().count() <= 120);
    if id.is_none() && person.is_none() {
        return Err(ApiError::invalid(
            "a nomination names a title, a person, or both",
        ));
    }
    let pid = person.as_deref().map(person_id).filter(|p| !p.is_empty());
    if let (Some(p), Some(name)) = (&pid, &person) {
        ctx.db.execute(
            "INSERT INTO people (id, name) VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING",
            params![p, name],
        )?;
    }
    ctx.db.execute(
        "INSERT INTO nominations (event_id, year, category, type, title_id, person_id, person, won, author_id, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![event, year, category, kind, id, pid, person, req.flag("won"), user.id, ctx.now],
    )?;
    Ok(Res::accepted(
        json!({ "id": ctx.db.last_insert_rowid(), "state": "pending" }),
    ))
}

pub fn title_awards(ctx: &Ctx, kind: &str, id: &str) -> Out {
    check_type(kind)?;
    check_id(id)?;
    let mut statement = ctx.db.prepare(
        "SELECT id, event_id, year, category, type, title_id, person, person_id, won FROM nominations
         WHERE type = ?1 AND title_id = ?2 AND state = 'approved' ORDER BY year DESC, won DESC",
    )?;
    let items = statement
        .query_map(params![kind, id], |r| nomination_json(ctx, r))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let wins = items
        .iter()
        .filter(|n| n["won"].as_bool() == Some(true))
        .count();
    Ok(Res::ok(
        json!({ "wins": wins, "nominations": items.len(), "items": items }),
    ))
}

// --------------------------------------------------------------------- polls

pub fn polls(ctx: &Ctx, req: &Req) -> Out {
    let mut statement = ctx.db.prepare(
        "SELECT id, question, at, (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id = polls.id) FROM polls
         WHERE state = 'approved' ORDER BY at DESC LIMIT ?1 OFFSET ?2",
    )?;
    let items = statement
        .query_map(params![PAGE + 1, req.page() * PAGE], |r| {
            Ok(json!({ "id": r.get::<_, i64>(0)?, "question": r.get::<_, String>(1)?, "at": r.get::<_, i64>(2)?, "total": r.get::<_, i64>(3)? }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (items, more) = paged(items, PAGE);
    Ok(Res::ok(json!({ "polls": items, "more": more })))
}

pub fn poll(ctx: &Ctx, poll: &str) -> Out {
    let id = number(poll)?;
    let row: Option<(String, Option<i64>, String)> = ctx
        .db
        .query_row(
            "SELECT question, author_id, state FROM polls WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((question, author, state)) = row else {
        return Err(ApiError::not_found());
    };
    if state != "approved" && (author.is_none() || author != ctx.uid()) {
        return Err(ApiError::not_found());
    }
    let mut statement = ctx.db.prepare(
        "SELECT idx, label, type, title_id, (SELECT COUNT(*) FROM poll_votes v WHERE v.poll_id = o.poll_id AND v.idx = o.idx)
         FROM poll_options o WHERE poll_id = ?1 ORDER BY idx",
    )?;
    let rows = statement
        .query_map(params![id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut options = Vec::new();
    let mut total = 0;
    for (index, label, kind, title_id, votes) in rows {
        total += votes;
        let title = match (&kind, &title_id) {
            (Some(k), Some(i)) => title_json(ctx.db, k, i)?,
            _ => Value::Null,
        };
        options.push(json!({ "index": index, "label": label, "type": kind, "id": title_id, "title": title, "votes": votes }));
    }
    let mine: Option<i64> = ctx
        .db
        .query_row(
            "SELECT idx FROM poll_votes WHERE poll_id = ?1 AND user_id = ?2",
            params![id, ctx.uid().unwrap_or(-1)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(Res::ok(json!({
        "id": id, "question": question, "state": state,
        "author": match author { Some(a) => Value::String(username(ctx.db, a)?), None => Value::Null },
        "options": options, "total": total, "mine": mine,
    })))
}

pub fn add_poll(ctx: &Ctx, req: &Req) -> Out {
    let user = ctx.writer()?;
    let question = req
        .text("question")
        .filter(|q| (10..=300).contains(&q.chars().count()))
        .ok_or_else(|| ApiError::invalid("a question is 10 to 300 characters"))?;
    let options = req
        .body
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !(2..=20).contains(&options.len()) {
        return Err(ApiError::invalid("a poll has 2 to 20 options"));
    }
    ctx.db.execute(
        "INSERT INTO polls (author_id, question, at) VALUES (?1, ?2, ?3)",
        params![user.id, question, ctx.now],
    )?;
    let poll = ctx.db.last_insert_rowid();
    for (index, option) in options.iter().enumerate() {
        let kind = option.get("type").and_then(Value::as_str);
        let id = option.get("id").and_then(Value::as_str);
        let label = option
            .get("label")
            .and_then(Value::as_str)
            .map(|l| l.trim().chars().take(200).collect::<String>());
        if let (Some(k), Some(i)) = (kind, id) {
            check_type(k)?;
            check_id(i)?;
            remember_title(ctx.db, k, i, option.get("title").unwrap_or(&Value::Null))?;
        } else if label.as_ref().is_none_or(String::is_empty) {
            return Err(ApiError::invalid("an option is a label or a title"));
        }
        ctx.db.execute(
            "INSERT INTO poll_options (poll_id, idx, label, type, title_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![poll, index as i64, label, kind, id],
        )?;
    }
    Ok(Res::accepted(json!({ "id": poll, "state": "pending" })))
}

pub fn vote_poll(ctx: &Ctx, req: &Req, poll: &str) -> Out {
    let user = ctx.writer()?;
    let id = number(poll)?;
    let option = req
        .body
        .get("option")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::invalid("say which option"))?;
    let valid: bool = ctx.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM poll_options o JOIN polls p ON p.id = o.poll_id WHERE o.poll_id = ?1 AND o.idx = ?2 AND p.state = 'approved')",
        params![id, option],
        |r| r.get(0),
    )?;
    if !valid {
        return Err(ApiError::not_found());
    }
    ctx.db.execute(
        "INSERT INTO poll_votes (poll_id, user_id, idx) VALUES (?1, ?2, ?3) ON CONFLICT DO UPDATE SET idx = excluded.idx",
        params![id, user.id, option],
    )?;
    self::poll(ctx, &id.to_string())
}
