// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The contract, end to end: a real server on a loopback port, a fresh
//! database, and every part of `community-api.md` exercised over HTTP.

#![allow(clippy::unwrap_used, clippy::panic, clippy::too_many_lines)]

use serde_json::{json, Value};

struct Server {
    base: String,
    db: String,
}

fn start(name: &str) -> Server {
    let dir =
        std::env::temp_dir().join(format!("eon-community-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("community.sqlite").to_string_lossy().into_owned();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let path = db_path.clone();
    std::thread::spawn(move || {
        let db = eon_stream_community::db::open(&path).unwrap();
        eon_stream_community::serve(&server, &db);
    });
    Server { base, db: db_path }
}

impl Server {
    fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = ureq::request(method, &format!("{}{path}", self.base));
        if let Some(token) = token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let result = match body {
            Some(body) => request
                .set("Content-Type", "application/json")
                .send_string(&body.to_string()),
            None => request.call(),
        };
        let response = match result {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("{method} {path}: {e}"),
        };
        let status = response.status();
        let text = response.into_string().unwrap_or_default();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn get(&self, path: &str, token: Option<&str>) -> Value {
        let (status, body) = self.call("GET", path, token, None);
        assert_eq!(status, 200, "GET {path}: {body}");
        body
    }

    fn account(&self, name: &str) -> String {
        let (status, body) = self.call(
            "POST",
            "/v0/accounts",
            None,
            Some(json!({ "username": name, "email": format!("{name}@example.org"), "password": "a long enough password" })),
        );
        assert_eq!(status, 201, "{body}");
        let (status, body) = self.call(
            "POST",
            "/v0/sessions",
            None,
            Some(json!({ "login": name, "password": "a long enough password" })),
        );
        assert_eq!(status, 200, "{body}");
        body["token"].as_str().unwrap().to_owned()
    }

    fn role(&self, name: &str, role: &str) {
        let db = rusqlite::Connection::open(&self.db).unwrap();
        db.execute(
            "UPDATE users SET role = ?1 WHERE username = ?2",
            rusqlite::params![role, name],
        )
        .unwrap();
    }

    fn rate(&self, token: &str, kind: &str, id: &str, score: i64, review: Option<&str>) {
        let (status, body) = self.call(
            "PUT",
            &format!("/v0/ratings/{kind}/{id}"),
            Some(token),
            Some(json!({
                "score": score, "review": review, "spoiler": false,
                "title": { "name": format!("Title {id}"), "year": "2020", "poster": "https://img.example.org/p.jpg",
                           "genres": ["Drama", "Isekai"], "cast": ["Bryan Cranston", "Aaron Paul"], "director": ["Vince Gilligan"] },
            })),
        );
        assert_eq!(status, 200, "{body}");
    }
}

#[test]
fn accounts_sessions_and_the_data_a_user_can_take_away() {
    let s = start("accounts");
    let token = s.account("ayse");
    // Taken, too short, wrong password, bad username.
    let (status, body) = s.call(
        "POST",
        "/v0/accounts",
        None,
        Some(json!({ "username": "ayse", "email": "x@example.org", "password": "0123456789" })),
    );
    assert_eq!((status, body["error"].as_str()), (409, Some("taken")));
    let (status, _) = s.call(
        "POST",
        "/v0/accounts",
        None,
        Some(json!({ "username": "kemal", "email": "k@example.org", "password": "short" })),
    );
    assert_eq!(status, 400);
    let (status, _) = s.call(
        "POST",
        "/v0/accounts",
        None,
        Some(json!({ "username": "Big Name", "email": "b@example.org", "password": "0123456789" })),
    );
    assert_eq!(status, 400);
    let (status, body) = s.call(
        "POST",
        "/v0/sessions",
        None,
        Some(json!({ "login": "ayse", "password": "not the password" })),
    );
    assert_eq!(
        (status, body["error"].as_str()),
        (401, Some("unauthorised"))
    );
    // Signing in by e-mail works too.
    let (status, _) = s.call(
        "POST",
        "/v0/sessions",
        None,
        Some(json!({ "login": "ayse@example.org", "password": "a long enough password" })),
    );
    assert_eq!(status, 200);

    let me = s.get("/v0/me", Some(&token));
    assert_eq!(me["email"], "ayse@example.org");
    let (status, me) = s.call(
        "PUT",
        "/v0/me",
        Some(&token),
        Some(json!({ "bio": "Anime and noir." })),
    );
    assert_eq!((status, me["bio"].as_str()), (200, Some("Anime and noir.")));
    // No one else sees the e-mail address.
    let page = s.get("/v0/users/ayse", None);
    assert!(!page.to_string().contains("example.org"));

    s.rate(
        &token,
        "movie",
        "tt1",
        9,
        Some("A film about a film about a film."),
    );
    let export = s.get("/v0/me/export", Some(&token));
    assert_eq!(export["ratings"].as_array().unwrap().len(), 1);
    assert_eq!(export["lists"][0]["kind"], "watchlist");
    assert!(
        !export.to_string().contains("hash"),
        "no password hash in an export"
    );

    // A changed password ends every session.
    let (status, _) = s.call(
        "PUT",
        "/v0/me/password",
        Some(&token),
        Some(json!({ "old": "a long enough password", "new": "another long password" })),
    );
    assert_eq!(status, 204);
    let (status, _) = s.call("GET", "/v0/me", Some(&token), None);
    assert_eq!(status, 401);
    let (_, body) = s.call(
        "POST",
        "/v0/sessions",
        None,
        Some(json!({ "login": "ayse", "password": "another long password" })),
    );
    let token = body["token"].as_str().unwrap().to_owned();

    let (status, _) = s.call("DELETE", "/v0/me", Some(&token), None);
    assert_eq!(status, 204);
    let (status, _) = s.call("GET", "/v0/users/ayse", None, None);
    assert_eq!(status, 404);
    let title = s.get("/v0/titles/movie/tt1", None);
    assert_eq!(title["count"], 0, "a deleted account's scores go with it");
    let (status, _) = s.call("GET", "/v0/admin", None, None);
    assert_eq!(status, 404);
}

#[test]
fn scores_reviews_charts_and_more_like_this() {
    let s = start("ratings");
    let users: Vec<String> = (0..7).map(|n| s.account(&format!("user{n}"))).collect();
    // Seven people: everyone likes tt1 and tt2, five like tt3, two dislike tt4.
    for (n, token) in users.iter().enumerate() {
        s.rate(
            token,
            "movie",
            "tt1",
            9,
            if n < 3 {
                Some("Wonderful from beginning to end.")
            } else {
                None
            },
        );
        s.rate(token, "movie", "tt2", 8, None);
        if n < 5 {
            s.rate(token, "movie", "tt3", 7, None);
        }
        if n < 2 {
            s.rate(token, "movie", "tt4", 2, None);
        }
    }
    let title = s.get("/v0/titles/movie/tt1", Some(&users[0]));
    assert_eq!(title["count"], 7);
    assert_eq!(title["average"], 9.0);
    assert_eq!(title["histogram"][8], 7);
    assert_eq!(title["mine"]["score"], 9);
    assert_eq!(title["reviews"].as_array().unwrap().len(), 3);
    assert_eq!(title["title"]["name"], "Title tt1");
    let few = s.get("/v0/titles/movie/tt4", None);
    assert_eq!(
        few["average"],
        Value::Null,
        "two scores are not a community's opinion"
    );

    // Helpful votes: never on one's own review; the order follows them.
    let (status, _) = s.call(
        "PUT",
        "/v0/reviews/user0/movie/tt1/helpful",
        Some(&users[0]),
        Some(json!({ "helpful": true })),
    );
    assert_eq!(status, 403);
    for voter in &users[3..7] {
        let (status, _) = s.call(
            "PUT",
            "/v0/reviews/user2/movie/tt1/helpful",
            Some(voter),
            Some(json!({ "helpful": true })),
        );
        assert_eq!(status, 200);
    }
    let (_, votes) = s.call(
        "PUT",
        "/v0/reviews/user1/movie/tt1/helpful",
        Some(&users[3]),
        Some(json!({ "helpful": false })),
    );
    assert_eq!(votes["unhelpful"], 1);
    let reviews = s.get("/v0/titles/movie/tt1/reviews?sort=helpful", None);
    assert_eq!(reviews["reviews"][0]["username"], "user2");
    let notes = s.get("/v0/me/notifications", Some(&users[2]));
    assert!(notes["unread"].as_i64().unwrap() >= 1);
    assert_eq!(notes["items"][0]["kind"], "helpful");

    // Charts: five scores to enter; tt4 has two.
    let top = s.get("/v0/top/movie", None);
    let ids: Vec<&str> = top["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["tt1", "tt2", "tt3"]);
    assert_eq!(top["items"][0]["rank"], 1);
    let bottom = s.get("/v0/bottom/movie", None);
    assert_eq!(bottom["items"][0]["id"], "tt3");
    let genre = s.get("/v0/top/movie?genre=Isekai", None);
    assert_eq!(genre["items"].as_array().unwrap().len(), 3);
    let none = s.get("/v0/top/movie?genre=Western", None);
    assert!(none["items"].as_array().unwrap().is_empty());
    let trending = s.get("/v0/trending/movie", None);
    assert_eq!(trending["items"][0]["id"], "tt1");

    // More like this, and recommendations from it.
    let similar = s.get("/v0/titles/movie/tt1/similar", None);
    let ids: Vec<&str> = similar["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.first().copied(), Some("tt2"));
    assert!(
        !ids.contains(&"tt4"),
        "a disliked title is not like a liked one"
    );
    let newcomer = s.account("newcomer");
    s.rate(&newcomer, "movie", "tt1", 10, None);
    let recommended = s.get("/v0/me/recommendations", Some(&newcomer));
    let ids: Vec<&str> = recommended["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.first().copied(), Some("tt2"));
    assert!(
        !ids.contains(&"tt1"),
        "what you scored is not recommended back"
    );

    // An episode is not rated; a bad score is refused.
    let (status, _) = s.call(
        "PUT",
        "/v0/ratings/series/tt9:1:1",
        Some(&users[0]),
        Some(json!({ "score": 5, "title": { "name": "x" } })),
    );
    assert_eq!(status, 400);
    let (status, _) = s.call(
        "PUT",
        "/v0/ratings/movie/tt5",
        Some(&users[0]),
        Some(json!({ "score": 11, "title": { "name": "x" } })),
    );
    assert_eq!(status, 400);
}

#[test]
fn facts_people_parental_guide_and_skip_markers() {
    let s = start("content");
    let writer = s.account("writer");
    let reader = s.account("reader");
    let moderator = s.account("moderator");
    s.role("moderator", "moderator");
    s.rate(&writer, "series", "tt0903747", 10, None);

    // A fact waits for a moderator; its author sees it, no one else does.
    let (status, added) = s.call(
        "POST",
        "/v0/titles/series/tt0903747/facts",
        Some(&writer),
        Some(json!({ "kind": "trivia", "text": "The pink teddy bear appears in several cold opens.", "spoiler": false })),
    );
    assert_eq!((status, added["state"].as_str()), (202, Some("pending")));
    assert_eq!(
        s.get("/v0/titles/series/tt0903747/facts", Some(&writer))["facts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        s.get("/v0/titles/series/tt0903747/facts", Some(&reader))["facts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, _) = s.call("GET", "/v0/mod/queue", Some(&reader), None);
    assert_eq!(status, 403);
    let queue = s.get("/v0/mod/queue?kind=fact", Some(&moderator));
    let item = queue["items"][0]["item"].as_str().unwrap().to_owned();
    let (status, _) = s.call(
        "POST",
        &format!("/v0/mod/queue/{item}"),
        Some(&moderator),
        Some(json!({ "decision": "approve", "note": "Checked." })),
    );
    assert_eq!(status, 200);
    let facts = s.get(
        "/v0/titles/series/tt0903747/facts?kind=trivia",
        Some(&reader),
    );
    assert_eq!(facts["counts"]["trivia"], 1);
    let fact = facts["facts"][0]["id"].as_i64().unwrap();
    let (_, votes) = s.call(
        "PUT",
        &format!("/v0/facts/{fact}/vote"),
        Some(&reader),
        Some(json!({ "interesting": true })),
    );
    assert_eq!(votes["interesting"], 1);
    let (status, _) = s.call(
        "PUT",
        &format!("/v0/facts/{fact}/vote"),
        Some(&writer),
        Some(json!({ "interesting": true })),
    );
    assert_eq!(status, 403);
    // A short fact and an unknown kind are refused.
    let (status, _) = s.call(
        "POST",
        "/v0/titles/series/tt0903747/facts",
        Some(&writer),
        Some(json!({ "kind": "trivia", "text": "short" })),
    );
    assert_eq!(status, 400);
    let (status, _) = s.call(
        "POST",
        "/v0/titles/series/tt0903747/facts",
        Some(&writer),
        Some(json!({ "kind": "gossip", "text": "Something long enough to count." })),
    );
    assert_eq!(status, 400);

    // People: the score's cast made a page with a filmography.
    let person = s.get("/v0/people/bryan-cranston", None);
    assert_eq!(person["name"], "Bryan Cranston");
    assert_eq!(person["filmography"][0]["id"], "tt0903747");
    let found = s.get("/v0/people?q=bryan", None);
    assert_eq!(found["items"][0]["id"], "bryan-cranston");
    let (status, _) = s.call(
        "POST",
        "/v0/people/bryan-cranston/edits",
        Some(&writer),
        Some(json!({ "field": "born", "value": "7 March 1956" })),
    );
    assert_eq!(status, 202);
    let queue = s.get("/v0/mod/queue?kind=edit", Some(&moderator));
    let item = queue["items"][0]["item"].as_str().unwrap().to_owned();
    s.call(
        "POST",
        &format!("/v0/mod/queue/{item}"),
        Some(&moderator),
        Some(json!({ "decision": "approve" })),
    );
    assert_eq!(
        s.get("/v0/people/bryan-cranston", None)["born"],
        "7 March 1956"
    );
    let (status, _) = s.call(
        "POST",
        "/v0/people/bryan-cranston/facts",
        Some(&writer),
        Some(json!({ "kind": "trivia", "text": "Was a dating-show contestant before acting." })),
    );
    assert_eq!(status, 202);

    // Parental guide: a verdict from three votes.
    for token in [&writer, &reader, &moderator] {
        let (status, _) = s.call(
            "PUT",
            "/v0/titles/series/tt0903747/parental/violence",
            Some(token),
            Some(json!({ "level": "severe" })),
        );
        assert_eq!(status, 200);
    }
    let guide = s.get("/v0/titles/series/tt0903747/parental", Some(&reader));
    let violence = guide["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "violence")
        .unwrap();
    assert_eq!(violence["verdict"], "severe");
    assert_eq!(violence["mine"], "severe");
    let (status, _) = s.call(
        "PUT",
        "/v0/titles/series/tt0903747/parental/violence",
        Some(&reader),
        Some(json!({ "level": "extreme" })),
    );
    assert_eq!(status, 400);

    // Skip markers: a near duplicate is a vote, not a second marker.
    let (status, first) = s.call(
        "POST",
        "/v0/segments/series/tt0903747:1:1",
        Some(&writer),
        Some(json!({ "kind": "intro", "start": 60.0, "end": 95.0, "duration": 2900.0 })),
    );
    assert_eq!(status, 201);
    let (_, second) = s.call(
        "POST",
        "/v0/segments/series/tt0903747:1:1",
        Some(&reader),
        Some(json!({ "kind": "intro", "start": 61.0, "end": 96.0, "duration": 2900.0 })),
    );
    assert_eq!(first["id"], second["id"]);
    let segments = s.get("/v0/segments/series/tt0903747:1:1", None);
    assert_eq!(segments["segments"][0]["score"], 2);
    let (status, _) = s.call(
        "POST",
        "/v0/segments/series/tt0903747:1:1",
        Some(&writer),
        Some(json!({ "kind": "intro", "start": 100.0, "end": 90.0, "duration": 2900.0 })),
    );
    assert_eq!(status, 400);
}

#[test]
fn lists_awards_polls_friends_and_moderation() {
    let s = start("social");
    let ayse = s.account("ayse");
    let mehmet = s.account("mehmet");
    let first_mod = s.account("firstmod");
    let second_mod = s.account("secondmod");
    s.role("firstmod", "moderator");
    s.role("secondmod", "admin");

    // Lists: private stays private; a like notifies the owner.
    let (status, list) = s.call("POST", "/v0/lists", Some(&ayse), Some(json!({ "name": "Best isekai", "description": "In order.", "public": true, "ranked": true })));
    assert_eq!(status, 201);
    let list = list["id"].as_i64().unwrap();
    let (status, _) = s.call(
        "PUT",
        &format!("/v0/lists/{list}/items/series/tt5"),
        Some(&ayse),
        Some(json!({ "title": { "name": "Re:Zero" }, "note": "The best.", "position": 1 })),
    );
    assert_eq!(status, 200);
    let (status, _) = s.call(
        "PUT",
        &format!("/v0/lists/{list}/items/series/tt5"),
        Some(&mehmet),
        Some(json!({ "title": { "name": "x" } })),
    );
    assert_eq!(status, 403);
    let (_, private) = s.call(
        "POST",
        "/v0/lists",
        Some(&ayse),
        Some(json!({ "name": "Secret", "public": false })),
    );
    let private = private["id"].as_i64().unwrap();
    let (status, _) = s.call("GET", &format!("/v0/lists/{private}"), Some(&mehmet), None);
    assert_eq!(status, 404);
    let (_, likes) = s.call(
        "PUT",
        &format!("/v0/lists/{list}/like"),
        Some(&mehmet),
        None,
    );
    assert_eq!(likes["likes"], 1);
    assert_eq!(
        s.get("/v0/me/notifications", Some(&ayse))["items"][0]["kind"],
        "list_like"
    );
    let shown = s.get(&format!("/v0/lists/{list}"), None);
    assert_eq!(shown["items"][0]["title"]["name"], "Re:Zero");
    assert_eq!(
        s.get("/v0/titles/series/tt5/lists", None)["lists"][0]["id"],
        list
    );
    let mine = s.get("/v0/lists?owner=ayse", Some(&ayse));
    assert_eq!(
        mine["lists"].as_array().unwrap().len(),
        3,
        "two lists and the watchlist"
    );
    let theirs = s.get("/v0/lists?owner=ayse", Some(&mehmet));
    assert_eq!(theirs["lists"].as_array().unwrap().len(), 1);
    let watchlist = s.get("/v0/me/watchlist", Some(&ayse));
    let (status, _) = s.call(
        "DELETE",
        &format!("/v0/lists/{}", watchlist["id"]),
        Some(&ayse),
        None,
    );
    assert_eq!(status, 403);

    // Awards: a nomination, approved, on the year and the title.
    let (status, _) = s.call("POST", "/v0/awards/oscars/2024", Some(&ayse), Some(json!({
        "event_name": "Academy Awards", "category": "Best Picture", "type": "movie", "id": "tt15398776",
        "title": { "name": "Oppenheimer" }, "won": true })));
    assert_eq!(status, 202);
    let queue = s.get("/v0/mod/queue?kind=nomination", Some(&first_mod));
    let item = queue["items"][0]["item"].as_str().unwrap().to_owned();
    s.call(
        "POST",
        &format!("/v0/mod/queue/{item}"),
        Some(&first_mod),
        Some(json!({ "decision": "approve" })),
    );
    assert_eq!(s.get("/v0/awards", None)["events"][0]["years"][0], 2024);
    assert_eq!(
        s.get("/v0/awards/oscars/2024", None)["categories"][0]["nominations"][0]["title"]["name"],
        "Oppenheimer"
    );
    assert_eq!(s.get("/v0/titles/movie/tt15398776/awards", None)["wins"], 1);

    // Polls.
    let (status, poll) = s.call("POST", "/v0/polls", Some(&ayse), Some(json!({
        "question": "Which ending was the best?", "options": [{ "label": "The first" }, { "type": "movie", "id": "tt1", "title": { "name": "Another" } }] })));
    assert_eq!(status, 202);
    let poll = poll["id"].as_i64().unwrap();
    let (status, _) = s.call(
        "PUT",
        &format!("/v0/polls/{poll}/vote"),
        Some(&mehmet),
        Some(json!({ "option": 0 })),
    );
    assert_eq!(status, 404, "not open before a moderator approves it");
    s.call(
        "POST",
        &format!("/v0/mod/queue/poll-{poll}"),
        Some(&first_mod),
        Some(json!({ "decision": "approve" })),
    );
    let (_, voted) = s.call(
        "PUT",
        &format!("/v0/polls/{poll}/vote"),
        Some(&mehmet),
        Some(json!({ "option": 1 })),
    );
    assert_eq!(
        (voted["total"].as_i64(), voted["mine"].as_i64()),
        (Some(1), Some(1))
    );
    assert_eq!(s.get("/v0/polls", None)["polls"][0]["id"], poll);

    // Friends and the feed.
    let (_, asked) = s.call("POST", "/v0/friends/mehmet", Some(&ayse), None);
    assert_eq!(asked["state"], "asked");
    assert_eq!(s.get("/v0/friends", Some(&mehmet))["incoming"][0], "ayse");
    let (_, accepted) = s.call("POST", "/v0/friends/ayse", Some(&mehmet), None);
    assert_eq!(accepted["state"], "friends");
    s.rate(
        &mehmet,
        "movie",
        "tt7",
        8,
        Some("Surprisingly tender, all the way through."),
    );
    let feed = s.get("/v0/feed", Some(&ayse));
    assert_eq!(feed[0]["username"], "mehmet");
    assert_eq!(s.get("/v0/users?q=meh", None)[0]["username"], "mehmet");
    assert_eq!(s.get("/v0/users/mehmet", Some(&ayse))["friend"], "friends");

    // Reported, removed, told, appealed, restored -- by a different moderator.
    let (status, _) = s.call("POST", "/v0/reports", Some(&ayse), Some(json!({ "kind": "review", "target": { "username": "mehmet", "type": "movie", "id": "tt7" }, "reason": "Spoils the ending without a warning." })));
    assert_eq!(status, 202);
    let queue = s.get("/v0/mod/queue?kind=report", Some(&first_mod));
    let item = queue["items"][0]["item"].as_str().unwrap().to_owned();
    assert!(queue["items"][0]["content"]["reported"]["review"]["review"].is_string());
    let (_, decided) = s.call(
        "POST",
        &format!("/v0/mod/queue/{item}"),
        Some(&first_mod),
        Some(json!({ "decision": "remove", "note": "Spoiler without a mark." })),
    );
    let decision = decided["log"].as_i64().unwrap();
    assert!(s.get("/v0/titles/movie/tt7", None)["reviews"]
        .as_array()
        .unwrap()
        .is_empty());
    let told = s.get("/v0/me/notifications", Some(&ayse));
    assert!(told["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["kind"] == "decision" && n["body"]["outcome"] == "upheld"));
    let (status, _) = s.call(
        "POST",
        "/v0/appeals",
        Some(&ayse),
        Some(json!({ "decision": decision, "text": "It was not my review to appeal." })),
    );
    assert_eq!(status, 403);
    let (status, appeal) = s.call(
        "POST",
        "/v0/appeals",
        Some(&mehmet),
        Some(json!({ "decision": decision, "text": "The ending is in the trailer already." })),
    );
    assert_eq!(status, 202);
    let (status, _) = s.call(
        "POST",
        "/v0/appeals",
        Some(&mehmet),
        Some(json!({ "decision": decision, "text": "And once more, with feeling." })),
    );
    assert_eq!(status, 409);
    let appeal_item = format!("appeal-{}", appeal["id"]);
    let own = s.get("/v0/mod/queue?kind=appeal", Some(&first_mod));
    assert!(
        own["items"].as_array().unwrap().is_empty(),
        "not shown to the moderator who decided"
    );
    let (status, _) = s.call(
        "POST",
        &format!("/v0/mod/queue/{appeal_item}"),
        Some(&first_mod),
        Some(json!({ "decision": "approve" })),
    );
    assert_eq!(status, 403);
    let (status, _) = s.call(
        "POST",
        &format!("/v0/mod/queue/{appeal_item}"),
        Some(&second_mod),
        Some(json!({ "decision": "approve", "note": "Fair." })),
    );
    assert_eq!(status, 200);
    assert_eq!(
        s.get("/v0/titles/movie/tt7", None)["reviews"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let log = s.get("/v0/mod/log", Some(&second_mod));
    assert!(log["entries"].as_array().unwrap().len() >= 2);

    // Suspension: reading yes, writing no.
    let (status, _) = s.call(
        "PUT",
        "/v0/mod/users/mehmet",
        Some(&first_mod),
        Some(json!({ "suspended_until": 4_102_444_800_i64, "note": "Test." })),
    );
    assert_eq!(status, 200);
    let (status, body) = s.call(
        "PUT",
        "/v0/ratings/movie/tt8",
        Some(&mehmet),
        Some(json!({ "score": 5, "title": { "name": "x" } })),
    );
    assert_eq!((status, body["error"].as_str()), (403, Some("suspended")));
    assert_eq!(s.call("GET", "/v0/me/export", Some(&mehmet), None).0, 200);

    // Roles: only an administrator gives them.
    let (status, _) = s.call(
        "PUT",
        "/v0/mod/roles/ayse",
        Some(&first_mod),
        Some(json!({ "role": "moderator" })),
    );
    assert_eq!(status, 403);
    let (status, _) = s.call(
        "PUT",
        "/v0/mod/roles/ayse",
        Some(&second_mod),
        Some(json!({ "role": "moderator" })),
    );
    assert_eq!(status, 200);

    // Blocking ends a friendship and stops new requests.
    let (status, _) = s.call("PUT", "/v0/blocks/mehmet", Some(&ayse), None);
    assert_eq!(status, 204);
    assert!(s.get("/v0/friends", Some(&ayse))["friends"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn a_proxied_visitor_is_rate_limited_and_the_operator_is_not() {
    let s = start("limits");
    let hit = |forwarded: Option<&str>| {
        let mut request = ureq::get(&format!("{}/v0/top/movie", s.base));
        if let Some(ip) = forwarded {
            request = request.set("X-Forwarded-For", ip);
        }
        match request.call() {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r.status(),
            Err(e) => panic!("{e}"),
        }
    };
    let statuses: Vec<u16> = (0..250)
        .map(|_| hit(Some("198.51.100.1, 203.0.113.9")))
        .collect();
    assert_eq!(statuses.iter().filter(|s| **s == 429).count(), 10);
    // Another visitor behind the same proxy has a bucket of their own.
    assert_eq!(hit(Some("203.0.113.10")), 200);
    // The operator, on the machine, is not limited.
    assert!((0..260).all(|_| hit(None) == 200));
}
