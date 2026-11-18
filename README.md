<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# eon-stream-community

The reference server for EON Stream's
[community API](https://github.com/EON-Extensible-Open-Network/eon-stream-spec/blob/main/docs/community-api.md):
a film and series database run by the people who use it. Scores and reviews, the facts
people add (trivia, goofs, quotes, soundtracks, filming locations…), people's pages with
their filmographies, lists and watchlists, Top 250 and trending charts, awards, a
parental guide, polls, "skip intro" markers, friends — and the moderation all of that
needs.

**Nobody runs this for you, and the EON project does not run it at all.** Madde 30 of the
project plan forbids the project operating a community server before its Faz L1 gate is
met — a legal entity among the five conditions — because accounts and user-written text
are where data-protection and hosting-provider obligations begin. This repository exists
so that the contract has an open implementation anyone can read, test and run for their
own community. Whoever runs one is its operator, with those obligations: in Turkey, the
data controller under KVKK and the hosting provider under 5651.

## Run it

One command, with Rust installed:

```
cargo run --release -- serve
```

That listens on `http://127.0.0.1:8770` and keeps everything in `community.sqlite` in the
current directory. Then make yourself the first administrator:

```
eon-community role <your-username> admin
```

In EON Stream, open **Community**, enter `http://127.0.0.1:8770` and sign up. The client
refuses plain `http://` for anything but this machine, so before anyone else uses the
server, put it behind a TLS-terminating proxy (Caddy, nginx) and give people the
`https://` address.

The proxy must pass the visitor's address in `X-Forwarded-For` (Caddy and nginx's usual
configurations do): the server rate-limits per address, believes that header only from a
loopback peer, and does not limit requests made on the machine itself without it.

```
eon-community serve [--listen 127.0.0.1:8770] [--db community.sqlite]
eon-community role <username> <user|moderator|admin> [--db community.sqlite]
```

## What it keeps

Exactly what the contract allows (madde 31):

- an account: username, e-mail, an Argon2id password hash, an optional bio, a role;
- sessions, as SHA-256 hashes of their tokens — a copy of the database is not a set of
  logins;
- what people chose to publish: scores, reviews, facts, votes, lists, poll answers, skip
  markers, reports and appeals;
- the titles and people those were about, as clients described them — names, years,
  poster *addresses*, genres, cast and directors. It stores no images and fetches
  nothing.

**It keeps nothing about what anyone watched.** There is no history table, and CI fails
the build if one appears. Every account can export everything held about it
(`GET /v0/me/export`) and delete itself (`DELETE /v0/me`); deletion takes everything it
wrote with it, and the moderation log keeps only that a decision was made.

## Moderation

Facts, people's details, award nominations and polls wait in a queue until a moderator
approves them. Reviews, lists and users can be reported; a moderator decides each report
with a note, the reporter is told the outcome, and the person a decision concerns can
appeal it once — to a different moderator. Every decision is in `GET /v0/mod/log`.
Suspended accounts can sign in, read and export, but not write.

## Design

One process, one SQLite file, requests answered in order by
[`tiny_http`](https://crates.io/crates/tiny_http) — the size a reference server should
be: readable in an afternoon, with no async runtime and seven dependencies, each justified
in `Cargo.toml` (madde 37). A community large enough to outgrow that has outgrown a
reference server too.

- `src/db.rs` — the schema.
- `src/http.rs` — the transport rules: JSON, the error shape, body limits.
- `src/api/` — one module per part of the contract.
- `tests/api.rs` — the contract end to end, over HTTP, against a fresh database.

## Licence

`AGPL-3.0-or-later`. A server is where copyleft has teeth: anyone who runs a modified
version for others must offer them its source.
