// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Scores and reviews, the charts made of them, and "more like this".

use std::collections::HashMap;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::http::{ApiError, Out, Req, Res};
use crate::{
    blocked, check_id, check_type, notify, paged, remember_title, title_json, user_id, Ctx, PAGE,
};

/// Bayesian prior weight, in scores: the contract's "weight of five scores".
const PRIOR: f64 = 5.0;
/// A chart lists titles with at least this many scores.
const CHART_MIN: i64 = 5;

pub fn put(ctx: &Ctx, req: &Req, kind: &str, id: &str) -> Out {
    let user = ctx.writer()?;
    check_type(kind)?;
    check_id(id)?;
    let score = req
        .body
        .get("score")
        .and_then(Value::as_i64)
        .filter(|s| (1..=10).contains(s))
        .ok_or_else(|| ApiError::invalid("a score is 1 to 10"))?;
    let review = req.text("review");
    if review.as_ref().is_some_and(|r| r.chars().count() > 5000) {
        return Err(ApiError::invalid("a review is up to 5000 characters"));
    }
    if id.contains(':') {
        return Err(ApiError::invalid("episodes are not rated, only titles"));
    }
    remember_title(
        ctx.db,
        kind,
        id,
        req.body.get("title").unwrap_or(&Value::Null),
    )?;
    ctx.db.execute(
        "INSERT INTO ratings (user_id, type, id, score, review, spoiler, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(user_id, type, id) DO UPDATE SET score = excluded.score, review = excluded.review,
           spoiler = excluded.spoiler, at = excluded.at",
        params![user.id, kind, id, score, review, req.flag("spoiler"), ctx.now],
    )?;
    Ok(Res::ok(json!({
        "type": kind, "id": id, "score": score, "review": review, "spoiler": req.flag("spoiler"),
        "title": title_json(ctx.db, kind, id)?, "at": ctx.now,
    })))
}

pub fn delete(ctx: &Ctx, kind: &str, id: &str) -> Out {
    let user = ctx.me()?;
    ctx.db.execute(
        "DELETE FROM ratings WHERE user_id = ?1 AND type = ?2 AND id = ?3",
        params![user.id, kind, id],
    )?;
    ctx.db.execute(
        "DELETE FROM helpful WHERE author_id = ?1 AND type = ?2 AND id = ?3",
        params![user.id, kind, id],
    )?;
    Ok(Res::empty())
}

/// One review, as the contract shows it.
struct Review {
    author_id: i64,
    value: Value,
    helpful: i64,
    unhelpful: i64,
    at: i64,
}

/// The lower bound of the Wilson score interval: how sure one can be that a
/// review is helpful, not just how often it was called so.
#[must_use]
pub fn wilson(up: i64, down: i64) -> f64 {
    let n = (up + down) as f64;
    if n == 0.0 {
        return 0.0;
    }
    let z = 1.96_f64;
    let p = up as f64 / n;
    (p + z * z / (2.0 * n) - z * ((p * (1.0 - p) + z * z / (4.0 * n)) / n).sqrt())
        / (1.0 + z * z / n)
}

fn load_reviews(ctx: &Ctx, kind: &str, id: &str) -> Result<Vec<Review>, ApiError> {
    let mut statement = ctx.db.prepare(
        "SELECT r.user_id, u.username, r.score, r.review, r.spoiler, r.at,
           (SELECT COUNT(*) FROM helpful h WHERE h.author_id = r.user_id AND h.type = r.type AND h.id = r.id AND h.helpful = 1),
           (SELECT COUNT(*) FROM helpful h WHERE h.author_id = r.user_id AND h.type = r.type AND h.id = r.id AND h.helpful = 0),
           (SELECT helpful FROM helpful h WHERE h.author_id = r.user_id AND h.type = r.type AND h.id = r.id AND h.voter_id = ?3)
         FROM ratings r JOIN users u ON u.id = r.user_id
         WHERE r.type = ?1 AND r.id = ?2 AND r.review IS NOT NULL AND r.hidden = 0",
    )?;
    let viewer = ctx.uid().unwrap_or(-1);
    let mut out = Vec::new();
    for row in statement.query_map(params![kind, id, viewer], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, bool>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, Option<bool>>(8)?,
        ))
    })? {
        let (author_id, username, score, review, spoiler, at, helpful, unhelpful, mine) = row?;
        if viewer >= 0 && blocked(ctx.db, viewer, author_id)? {
            continue;
        }
        out.push(Review {
            author_id,
            value: json!({
                "username": username, "score": score, "review": review, "spoiler": spoiler, "at": at,
                "helpful": helpful, "unhelpful": unhelpful,
                "my_vote": mine.map(|m| if m { "helpful" } else { "unhelpful" }),
            }),
            helpful,
            unhelpful,
            at,
        });
    }
    Ok(out)
}

fn sorted_page(mut reviews: Vec<Review>, sort: &str, page: i64) -> (Vec<Value>, bool) {
    if sort == "newest" {
        reviews.sort_by_key(|a| std::cmp::Reverse(a.at));
    } else {
        reviews.sort_by(|a, b| {
            wilson(b.helpful, b.unhelpful)
                .total_cmp(&wilson(a.helpful, a.unhelpful))
                .then(b.at.cmp(&a.at))
        });
    }
    let start = (page * PAGE) as usize;
    let items: Vec<Value> = reviews
        .into_iter()
        .skip(start)
        .take(PAGE as usize + 1)
        .map(|r| {
            let _ = r.author_id;
            r.value
        })
        .collect();
    paged(items, PAGE)
}

pub fn title(ctx: &Ctx, req: &Req, kind: &str, id: &str) -> Out {
    check_type(kind)?;
    check_id(id)?;
    let mut histogram = [0_i64; 10];
    let mut statement = ctx.db.prepare(
        "SELECT score, COUNT(*) FROM ratings WHERE type = ?1 AND id = ?2 AND hidden = 0 GROUP BY score",
    )?;
    for row in statement.query_map(params![kind, id], |r| {
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
    let average = (count >= 3).then(|| (total as f64 / count as f64 * 10.0).round() / 10.0);
    let (reviews, more) = sorted_page(
        load_reviews(ctx, kind, id)?,
        req.q("sort").unwrap_or("helpful"),
        0,
    );
    let mine = match ctx.uid() {
        Some(me) => ctx
            .db
            .query_row(
                "SELECT score, review, spoiler, at FROM ratings WHERE user_id = ?1 AND type = ?2 AND id = ?3",
                params![me, kind, id],
                |r| {
                    Ok(json!({
                        "score": r.get::<_, i64>(0)?, "review": r.get::<_, Option<String>>(1)?,
                        "spoiler": r.get::<_, bool>(2)?, "at": r.get::<_, i64>(3)?,
                    }))
                },
            )
            .optional()?,
        None => None,
    };
    Ok(Res::ok(json!({
        "title": title_json(ctx.db, kind, id)?,
        "average": average, "count": count, "histogram": histogram,
        "reviews": reviews, "more": more, "mine": mine,
    })))
}

pub fn reviews(ctx: &Ctx, req: &Req, kind: &str, id: &str) -> Out {
    check_type(kind)?;
    check_id(id)?;
    let (reviews, more) = sorted_page(
        load_reviews(ctx, kind, id)?,
        req.q("sort").unwrap_or("helpful"),
        req.page(),
    );
    Ok(Res::ok(json!({ "reviews": reviews, "more": more })))
}

pub fn helpful(ctx: &Ctx, req: &Req, author: &str, kind: &str, id: &str, set: bool) -> Out {
    let user = ctx.writer()?;
    let author_id = user_id(ctx.db, author)?;
    if author_id == user.id {
        return Err(ApiError::forbidden("no one votes on their own review"));
    }
    let exists: bool = ctx.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ratings WHERE user_id = ?1 AND type = ?2 AND id = ?3 AND review IS NOT NULL)",
        params![author_id, kind, id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(ApiError::not_found());
    }
    if set {
        let value = req
            .body
            .get("helpful")
            .and_then(Value::as_bool)
            .ok_or_else(|| ApiError::invalid("say whether it was helpful"))?;
        let before: Option<bool> = ctx
            .db
            .query_row(
                "SELECT helpful FROM helpful WHERE voter_id = ?1 AND author_id = ?2 AND type = ?3 AND id = ?4",
                params![user.id, author_id, kind, id],
                |r| r.get(0),
            )
            .optional()?;
        ctx.db.execute(
            "INSERT INTO helpful (voter_id, author_id, type, id, helpful) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT DO UPDATE SET helpful = excluded.helpful",
            params![user.id, author_id, kind, id, value],
        )?;
        if value && before.is_none() {
            notify(
                ctx.db,
                author_id,
                "helpful",
                &json!({ "username": user.username, "type": kind, "id": id, "title": title_json(ctx.db, kind, id)? }),
                ctx.now,
            )?;
        }
    } else {
        ctx.db.execute(
            "DELETE FROM helpful WHERE voter_id = ?1 AND author_id = ?2 AND type = ?3 AND id = ?4",
            params![user.id, author_id, kind, id],
        )?;
    }
    let (up, down): (i64, i64) = ctx.db.query_row(
        "SELECT COALESCE(SUM(helpful = 1), 0), COALESCE(SUM(helpful = 0), 0) FROM helpful
         WHERE author_id = ?1 AND type = ?2 AND id = ?3",
        params![author_id, kind, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(Res::ok(json!({ "helpful": up, "unhelpful": down })))
}

/// `top` and `bottom`: a Bayesian average over titles with enough scores.
pub fn chart(ctx: &Ctx, req: &Req, kind: &str, lowest: bool) -> Out {
    check_type(kind)?;
    let mean: f64 = ctx.db.query_row(
        "SELECT COALESCE(AVG(score), 0) FROM ratings WHERE type = ?1 AND hidden = 0",
        params![kind],
        |r| r.get(0),
    )?;
    let genre = req
        .q("genre")
        .map(|g| format!("%\"{}\"%", g.replace(['%', '_', '"'], "")));
    let mut statement = ctx.db.prepare(
        "SELECT r.id, COUNT(*), AVG(r.score) FROM ratings r JOIN titles t ON t.type = r.type AND t.id = r.id
         WHERE r.type = ?1 AND r.hidden = 0 AND (?2 IS NULL OR t.genres LIKE ?2)
         GROUP BY r.id HAVING COUNT(*) >= ?3",
    )?;
    let mut ranked: Vec<(String, i64, f64)> = statement
        .query_map(params![kind, genre, CHART_MIN], |r| {
            let count: i64 = r.get(1)?;
            let average: f64 = r.get(2)?;
            let n = count as f64;
            Ok((
                r.get::<_, String>(0)?,
                count,
                (n * average + PRIOR * mean) / (n + PRIOR),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ranked.sort_by(|a, b| {
        let order = b.2.total_cmp(&a.2).then(b.1.cmp(&a.1));
        if lowest {
            order.reverse()
        } else {
            order
        }
    });
    let page = req.page();
    let start = (page * PAGE) as usize;
    let mut items = Vec::new();
    for (at, (id, count, average)) in ranked
        .iter()
        .enumerate()
        .skip(start)
        .take(PAGE as usize + 1)
    {
        items.push(json!({
            "type": kind, "id": id, "title": title_json(ctx.db, kind, id)?,
            "average": (average * 10.0).round() / 10.0, "count": count, "rank": at + 1,
        }));
    }
    let (items, more) = paged(items, PAGE);
    Ok(Res::ok(json!({ "items": items, "more": more })))
}

fn activity(ctx: &Ctx, kind: &str, from: i64, to: i64) -> Result<Vec<(String, i64)>, ApiError> {
    let mut statement = ctx.db.prepare(
        "SELECT id, SUM(weight) AS w FROM (
           SELECT id, CASE WHEN review IS NULL THEN 1 ELSE 3 END AS weight FROM ratings
             WHERE type = ?1 AND at >= ?2 AND at < ?3 AND hidden = 0
           UNION ALL
           SELECT li.id, 2 FROM list_items li JOIN lists l ON l.id = li.list_id
             WHERE li.type = ?1 AND li.at >= ?2 AND li.at < ?3 AND l.public = 1
         ) GROUP BY id ORDER BY w DESC, id LIMIT 500",
    )?;
    let out = statement
        .query_map(params![kind, from, to], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// What people scored, reviewed and listed most this week.
pub fn trending(ctx: &Ctx, req: &Req, kind: &str) -> Out {
    check_type(kind)?;
    let week = 7 * 86_400;
    let now = activity(ctx, kind, ctx.now - week, ctx.now + 1)?;
    let before: HashMap<String, usize> = activity(ctx, kind, ctx.now - 2 * week, ctx.now - week)?
        .into_iter()
        .enumerate()
        .map(|(at, (id, _))| (id, at + 1))
        .collect();
    let start = (req.page() * PAGE) as usize;
    let mut items = Vec::new();
    for (at, (id, weight)) in now.iter().enumerate().skip(start).take(PAGE as usize + 1) {
        let rank = at + 1;
        items.push(json!({
            "type": kind, "id": id, "title": title_json(ctx.db, kind, id)?,
            "count": weight, "rank": rank,
            "change": before.get(id).map(|was| *was as i64 - rank as i64),
        }));
    }
    let (items, more) = paged(items, PAGE);
    Ok(Res::ok(json!({ "items": items, "more": more })))
}

/// Item-to-item: of the people who liked this, what else they liked.
fn similar_to(
    ctx: &Ctx,
    kind: &str,
    id: &str,
) -> Result<Vec<(String, String, f64, i64)>, ApiError> {
    let likers: i64 = ctx.db.query_row(
        "SELECT COUNT(*) FROM ratings WHERE type = ?1 AND id = ?2 AND score >= 7",
        params![kind, id],
        |r| r.get(0),
    )?;
    if likers == 0 {
        return Ok(Vec::new());
    }
    let mut statement = ctx.db.prepare(
        "SELECT r2.type, r2.id, COUNT(*) AS co,
           (SELECT COUNT(*) FROM ratings r3 WHERE r3.type = r2.type AND r3.id = r2.id AND r3.score >= 7)
         FROM ratings r1 JOIN ratings r2 ON r1.user_id = r2.user_id AND NOT (r2.type = r1.type AND r2.id = r1.id)
         WHERE r1.type = ?1 AND r1.id = ?2 AND r1.score >= 7 AND r2.score >= 7
         GROUP BY r2.type, r2.id HAVING co >= 2",
    )?;
    let mut out: Vec<(String, String, f64, i64)> = statement
        .query_map(params![kind, id], |r| {
            let co: i64 = r.get(2)?;
            let other: i64 = r.get(3)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                co as f64 / ((likers * other.max(1)) as f64).sqrt(),
                co,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    out.sort_by(|a, b| b.2.total_cmp(&a.2).then(b.3.cmp(&a.3)));
    out.truncate(20);
    Ok(out)
}

pub fn similar(ctx: &Ctx, kind: &str, id: &str) -> Out {
    check_type(kind)?;
    check_id(id)?;
    let mut items = Vec::new();
    for (k, i, score, count) in similar_to(ctx, kind, id)? {
        items.push(json!({
            "type": k, "id": i, "title": title_json(ctx.db, &k, &i)?,
            "score": (score * 1000.0).round() / 1000.0, "count": count,
        }));
    }
    Ok(Res::ok(json!({ "items": items })))
}

/// "More like this", added up over what this person scored 8 or more.
pub fn recommendations(ctx: &Ctx) -> Out {
    let user = ctx.me()?;
    let mut statement = ctx
        .db
        .prepare("SELECT type, id, score FROM ratings WHERE user_id = ?1 ORDER BY at DESC")?;
    let mine: Vec<(String, String, i64)> = statement
        .query_map(params![user.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rated: std::collections::HashSet<(String, String)> = mine
        .iter()
        .map(|(k, i, _)| (k.clone(), i.clone()))
        .collect();
    let mut total: HashMap<(String, String), f64> = HashMap::new();
    for (kind, id, score) in mine.iter().filter(|(_, _, s)| *s >= 8).take(30) {
        let weight = (*score as f64 - 7.0) / 3.0;
        for (k, i, s, _) in similar_to(ctx, kind, id)? {
            if !rated.contains(&(k.clone(), i.clone())) {
                *total.entry((k, i)).or_default() += s * weight;
            }
        }
    }
    let mut ranked: Vec<((String, String), f64)> = total.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut items = Vec::new();
    for ((k, i), score) in ranked.into_iter().take(40) {
        items.push(json!({ "type": k, "id": i, "title": title_json(ctx.db, &k, &i)?, "score": (score * 1000.0).round() / 1000.0 }));
    }
    Ok(Res::ok(json!({ "items": items })))
}

#[cfg(test)]
mod tests {
    use super::wilson;

    #[test]
    fn ninety_of_a_hundred_beats_three_of_three() {
        assert!(wilson(90, 10) > wilson(3, 0));
        assert!(wilson(0, 0).abs() < f64::EPSILON);
    }
}
