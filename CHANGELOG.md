<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# Changelog

## 0.2.0 — signing in with EON Account

Community API v0.1 (plan madde 46):

- **`POST /v0/sessions/account`** signs in with an assertion from an EON Account server.
  - The operator names the account servers the server trusts, each with its key:
    `--origin` and `--trust-account ISSUER,KID,PUBLIC-KEY`. Checking an assertion makes no
    request out.
  - The account here is keyed on the pseudonym the account server made for this server
    alone. It has no password and no e-mail.
  - A shared EON username becomes the name here if it is free.
- Password sign-in, password change and `GET /v0/me` handle such accounts: there is
  nothing to sign in with, nothing to change, and no e-mail address.
- `RUSTSEC-2026-0387` is ignored in `deny.toml`, with the reason it cannot be reached.

## 0.1.0 — first version

The reference server for community API v0 as of EON Stream `v0.3`:

- accounts and sessions (Argon2id hashes, hashed session tokens), export and deletion;
- scores and reviews with spoiler marks and helpful votes (Wilson-ordered);
- charts: top and bottom (Bayesian average, five-score minimum, by genre) and trending
  (seven days, with movement);
- "more like this" (item-to-item cosine over scores of 7 and up) and recommendations
  built from it;
- facts for titles and people (trivia, goofs, quotes, soundtracks, locations and more),
  moderated, with interest votes;
- people's pages: details through moderated edits, filmographies built from what clients
  said a title's cast and directors were;
- the parental guide (five categories, four levels, a verdict from three votes);
- skip markers for intros, recaps, credits and previews, voted;
- lists, ranked or not, public or private, liked; every account's watchlist;
- awards events, years and nominations, moderated;
- polls, moderated;
- friends, the friends' feed, blocking, notifications;
- reports, the moderation queue, suspensions, roles, the log, and appeals decided by a
  different moderator.

It is not deployed anywhere by the EON project, and may not be before madde 30's Faz L1
gate.
