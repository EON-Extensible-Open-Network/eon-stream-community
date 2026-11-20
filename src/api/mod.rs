// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Every method and path in `community-api.md`, and nothing else.

mod accounts;
mod content;
pub mod eon_account;
mod lists;
mod moderation;
mod ratings;
mod social;

use crate::http::{ApiError, Out, Req};
use crate::Ctx;

pub fn route(ctx: &Ctx, req: &Req) -> Out {
    let parts: Vec<&str> = req.parts.iter().map(String::as_str).collect();
    let Some((&"v0", rest)) = parts.split_first() else {
        return Err(ApiError::not_found());
    };
    match (req.method.as_str(), rest) {
        // accounts and sessions
        ("POST", ["accounts"]) => accounts::create(ctx, req),
        ("POST", ["sessions"]) => accounts::sign_in(ctx, req),
        ("POST", ["sessions", "account"]) => eon_account::sign_in(ctx, req),
        ("DELETE", ["sessions"]) => accounts::sign_out(ctx, req),
        ("GET", ["me"]) => accounts::me(ctx),
        ("PUT", ["me"]) => accounts::update(ctx, req),
        ("PUT", ["me", "password"]) => accounts::password(ctx, req),
        ("GET", ["me", "export"]) => accounts::export(ctx),
        ("DELETE", ["me"]) => accounts::delete(ctx),
        ("GET", ["me", "notifications"]) => accounts::notifications(ctx),
        ("POST", ["me", "notifications"]) => accounts::read_notifications(ctx, req),
        ("GET", ["me", "recommendations"]) => ratings::recommendations(ctx),
        ("GET", ["me", "watchlist"]) => lists::watchlist(ctx, req),

        // scores, reviews, charts, more like this
        ("PUT", ["ratings", kind, id]) => ratings::put(ctx, req, kind, id),
        ("DELETE", ["ratings", kind, id]) => ratings::delete(ctx, kind, id),
        ("GET", ["titles", kind, id]) => ratings::title(ctx, req, kind, id),
        ("GET", ["titles", kind, id, "reviews"]) => ratings::reviews(ctx, req, kind, id),
        ("PUT", ["reviews", user, kind, id, "helpful"]) => {
            ratings::helpful(ctx, req, user, kind, id, true)
        }
        ("DELETE", ["reviews", user, kind, id, "helpful"]) => {
            ratings::helpful(ctx, req, user, kind, id, false)
        }
        ("GET", ["top", kind]) => ratings::chart(ctx, req, kind, false),
        ("GET", ["bottom", kind]) => ratings::chart(ctx, req, kind, true),
        ("GET", ["trending", kind]) => ratings::trending(ctx, req, kind),
        ("GET", ["titles", kind, id, "similar"]) => ratings::similar(ctx, kind, id),

        // facts, parental guide, skip markers, people
        ("GET", ["titles", kind, id, "facts"]) => {
            content::facts(ctx, req, "title", &format!("{kind}:{id}"), Some((kind, id)))
        }
        ("POST", ["titles", kind, id, "facts"]) => content::add_fact(ctx, req, "title", kind, id),
        ("GET", ["people", person, "facts"]) => content::facts(ctx, req, "person", person, None),
        ("POST", ["people", person, "facts"]) => content::add_fact(ctx, req, "person", "", person),
        ("PUT", ["facts", fact, "vote"]) => content::vote_fact(ctx, req, fact, true),
        ("DELETE", ["facts", fact, "vote"]) => content::vote_fact(ctx, req, fact, false),
        ("DELETE", ["facts", fact]) => content::withdraw_fact(ctx, fact),
        ("GET", ["titles", kind, id, "parental"]) => content::parental(ctx, kind, id),
        ("PUT", ["titles", kind, id, "parental", category]) => {
            content::vote_parental(ctx, req, kind, id, category)
        }
        ("GET", ["segments", kind, id]) => content::segments(ctx, kind, id),
        ("POST", ["segments", kind, id]) => content::add_segment(ctx, req, kind, id),
        ("PUT", ["segments", segment, "vote"]) => content::vote_segment(ctx, req, segment),
        ("GET", ["people"]) => content::find_people(ctx, req),
        ("GET", ["people", person]) => content::person(ctx, person),
        ("POST", ["people", person, "edits"]) => content::edit_person(ctx, req, person),
        ("GET", ["awards"]) => content::award_events(ctx),
        ("GET", ["awards", event, year]) => content::award_year(ctx, event, year),
        ("POST", ["awards", event, year]) => content::nominate(ctx, req, event, year),
        ("GET", ["titles", kind, id, "awards"]) => content::title_awards(ctx, kind, id),
        ("GET", ["polls"]) => content::polls(ctx, req),
        ("GET", ["polls", poll]) => content::poll(ctx, poll),
        ("POST", ["polls"]) => content::add_poll(ctx, req),
        ("PUT", ["polls", poll, "vote"]) => content::vote_poll(ctx, req, poll),

        // lists
        ("GET", ["lists"]) => lists::find(ctx, req),
        ("POST", ["lists"]) => lists::create(ctx, req),
        ("GET", ["lists", list]) => lists::get(ctx, req, list),
        ("PUT", ["lists", list]) => lists::update(ctx, req, list),
        ("DELETE", ["lists", list]) => lists::delete(ctx, list),
        ("PUT", ["lists", list, "items", kind, id]) => lists::put_item(ctx, req, list, kind, id),
        ("DELETE", ["lists", list, "items", kind, id]) => lists::delete_item(ctx, list, kind, id),
        ("PUT", ["lists", list, "like"]) => lists::like(ctx, list, true),
        ("DELETE", ["lists", list, "like"]) => lists::like(ctx, list, false),
        ("GET", ["titles", kind, id, "lists"]) => lists::of_title(ctx, req, kind, id),

        // people you know
        ("GET", ["users"]) => social::find(ctx, req),
        ("GET", ["users", user]) => social::profile(ctx, user),
        ("GET", ["friends"]) => social::friends(ctx),
        ("POST", ["friends", user]) => social::befriend(ctx, user),
        ("DELETE", ["friends", user]) => social::unfriend(ctx, user),
        ("GET", ["feed"]) => social::feed(ctx),
        ("PUT", ["blocks", user]) => social::block(ctx, user, true),
        ("DELETE", ["blocks", user]) => social::block(ctx, user, false),

        // reports, moderation, appeals
        ("POST", ["reports"]) => moderation::report(ctx, req),
        ("GET", ["mod", "queue"]) => moderation::queue(ctx, req),
        ("POST", ["mod", "queue", item]) => moderation::decide(ctx, req, item),
        ("PUT", ["mod", "users", user]) => moderation::suspend(ctx, req, user),
        ("PUT", ["mod", "roles", user]) => moderation::role(ctx, req, user),
        ("GET", ["mod", "log"]) => moderation::log_page(ctx, req),
        ("POST", ["appeals"]) => moderation::appeal(ctx, req),

        _ => Err(ApiError::not_found()),
    }
}

/// A number in a path.
pub(crate) fn number(text: &str) -> Result<i64, ApiError> {
    text.parse().map_err(|_| ApiError::not_found())
}
