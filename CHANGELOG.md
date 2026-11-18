<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 EON contributors
-->

# Changelog

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
