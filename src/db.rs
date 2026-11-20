// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The database: one SQLite file, its schema, and opening it.
//!
//! What is here is exactly what `community-api.md` lets a server keep: an
//! account is a username, an e-mail, a password hash and an optional line of
//! bio. Everything else is what people chose to publish. There is no table
//! for what anyone watched, and there must never be one.

use rusqlite::Connection;

const SCHEMA: &str = r"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS users (
    id              INTEGER PRIMARY KEY,
    username        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    -- Both empty for an account that signs in with EON Account (madde 46).
    email           TEXT UNIQUE COLLATE NOCASE,
    hash            TEXT,
    role            TEXT NOT NULL DEFAULT 'user',
    bio             TEXT,
    created_at      INTEGER NOT NULL,
    suspended_until INTEGER
);

-- Which EON Account pseudonym is which account here. The subject is the
-- pseudonym the account server made for this server alone.
CREATE TABLE IF NOT EXISTS account_links (
    issuer  TEXT NOT NULL,
    subject TEXT NOT NULL,
    user_id INTEGER NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (issuer, subject)
);

CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);

-- The titles the server has heard of, as clients described them.
CREATE TABLE IF NOT EXISTS titles (
    type     TEXT NOT NULL,
    id       TEXT NOT NULL,
    name     TEXT NOT NULL,
    year     TEXT,
    poster   TEXT,
    genres   TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (type, id)
);

CREATE TABLE IF NOT EXISTS people (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    born       TEXT,
    died       TEXT,
    birthplace TEXT,
    bio        TEXT
);

CREATE TABLE IF NOT EXISTS credits (
    type      TEXT NOT NULL,
    id        TEXT NOT NULL,
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    role      TEXT NOT NULL,
    PRIMARY KEY (type, id, person_id, role)
);
CREATE INDEX IF NOT EXISTS credits_person ON credits(person_id);

CREATE TABLE IF NOT EXISTS ratings (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type    TEXT NOT NULL,
    id      TEXT NOT NULL,
    score   INTEGER NOT NULL,
    review  TEXT,
    spoiler INTEGER NOT NULL DEFAULT 0,
    at      INTEGER NOT NULL,
    hidden  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, type, id)
);
CREATE INDEX IF NOT EXISTS ratings_title ON ratings(type, id);
CREATE INDEX IF NOT EXISTS ratings_at ON ratings(at);

CREATE TABLE IF NOT EXISTS helpful (
    voter_id  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    author_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type      TEXT NOT NULL,
    id        TEXT NOT NULL,
    helpful   INTEGER NOT NULL,
    PRIMARY KEY (voter_id, author_id, type, id)
);

CREATE TABLE IF NOT EXISTS facts (
    id           INTEGER PRIMARY KEY,
    subject_kind TEXT NOT NULL,          -- 'title' or 'person'
    subject      TEXT NOT NULL,          -- 'type:id' or a person id
    kind         TEXT NOT NULL,
    text         TEXT NOT NULL,
    spoiler      INTEGER NOT NULL DEFAULT 0,
    author_id    INTEGER REFERENCES users(id) ON DELETE CASCADE,
    state        TEXT NOT NULL DEFAULT 'pending',
    at           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS facts_subject ON facts(subject_kind, subject, state);

CREATE TABLE IF NOT EXISTS fact_votes (
    fact_id     INTEGER NOT NULL REFERENCES facts(id) ON DELETE CASCADE,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    interesting INTEGER NOT NULL,
    PRIMARY KEY (fact_id, user_id)
);

CREATE TABLE IF NOT EXISTS person_edits (
    id        INTEGER PRIMARY KEY,
    person_id TEXT NOT NULL,
    name      TEXT NOT NULL,
    field     TEXT NOT NULL,
    value     TEXT,
    author_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
    state     TEXT NOT NULL DEFAULT 'pending',
    at        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS parental (
    user_id  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type     TEXT NOT NULL,
    id       TEXT NOT NULL,
    category TEXT NOT NULL,
    level    TEXT NOT NULL,
    PRIMARY KEY (user_id, type, id, category)
);

CREATE TABLE IF NOT EXISTS segments (
    id        INTEGER PRIMARY KEY,
    type      TEXT NOT NULL,
    item      TEXT NOT NULL,
    kind      TEXT NOT NULL,
    start     REAL NOT NULL,
    end       REAL NOT NULL,
    author_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS segments_item ON segments(type, item);

CREATE TABLE IF NOT EXISTS segment_votes (
    segment_id INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    up         INTEGER NOT NULL,
    PRIMARY KEY (segment_id, user_id)
);

CREATE TABLE IF NOT EXISTS lists (
    id          INTEGER PRIMARY KEY,
    owner_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT,
    public      INTEGER NOT NULL DEFAULT 1,
    ranked      INTEGER NOT NULL DEFAULT 0,
    kind        TEXT NOT NULL DEFAULT 'custom',
    hidden      INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS list_items (
    list_id  INTEGER NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    type     TEXT NOT NULL,
    id       TEXT NOT NULL,
    note     TEXT,
    position INTEGER NOT NULL,
    at       INTEGER NOT NULL,
    PRIMARY KEY (list_id, type, id)
);
CREATE INDEX IF NOT EXISTS list_items_title ON list_items(type, id);

CREATE TABLE IF NOT EXISTS list_likes (
    list_id INTEGER NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (list_id, user_id)
);

CREATE TABLE IF NOT EXISTS award_events (
    id   TEXT PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS nominations (
    id        INTEGER PRIMARY KEY,
    event_id  TEXT NOT NULL REFERENCES award_events(id),
    year      INTEGER NOT NULL,
    category  TEXT NOT NULL,
    type      TEXT,
    title_id  TEXT,
    person_id TEXT,
    person    TEXT,
    won       INTEGER NOT NULL DEFAULT 0,
    author_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
    state     TEXT NOT NULL DEFAULT 'pending',
    at        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS polls (
    id        INTEGER PRIMARY KEY,
    author_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
    question  TEXT NOT NULL,
    state     TEXT NOT NULL DEFAULT 'pending',
    at        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS poll_options (
    poll_id INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
    idx     INTEGER NOT NULL,
    label   TEXT,
    type    TEXT,
    title_id TEXT,
    PRIMARY KEY (poll_id, idx)
);

CREATE TABLE IF NOT EXISTS poll_votes (
    poll_id INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    idx     INTEGER NOT NULL,
    PRIMARY KEY (poll_id, user_id)
);

CREATE TABLE IF NOT EXISTS friends (
    from_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    to_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    at      INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id)
);

CREATE TABLE IF NOT EXISTS blocks (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    blocked_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, blocked_id)
);

CREATE TABLE IF NOT EXISTS notifications (
    id      INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind    TEXT NOT NULL,
    body    TEXT NOT NULL,
    at      INTEGER NOT NULL,
    read    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS reports (
    id          INTEGER PRIMARY KEY,
    reporter_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    kind        TEXT NOT NULL,
    target      TEXT NOT NULL,
    reason      TEXT NOT NULL,
    state       TEXT NOT NULL DEFAULT 'open',
    at          INTEGER NOT NULL
);

-- Kept when an account is deleted: that a decision was made, by whom, and
-- why. The subject is a description, not a link to deleted content.
CREATE TABLE IF NOT EXISTS mod_log (
    id           INTEGER PRIMARY KEY,
    moderator    TEXT NOT NULL,
    action       TEXT NOT NULL,
    item         TEXT NOT NULL,
    subject_user TEXT,
    note         TEXT,
    at           INTEGER NOT NULL,
    appealed     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS appeals (
    id      INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    log_id  INTEGER NOT NULL REFERENCES mod_log(id),
    text    TEXT NOT NULL,
    state   TEXT NOT NULL DEFAULT 'open',
    at      INTEGER NOT NULL
);
";

/// Open the database, creating its tables when they are not there.
pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch(SCHEMA)?;
    Ok(db)
}
