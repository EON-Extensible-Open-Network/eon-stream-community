// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! `eon-community` -- run a community server.
//!
//! ```text
//! eon-community serve [--listen 127.0.0.1:8770] [--db community.sqlite]
//!                     [--origin https://community.example.org --trust-account ISSUER,KID,PUBLIC-KEY]
//! eon-community role <username> <user|moderator|admin> [--db community.sqlite]
//! ```
//!
//! It listens on plain HTTP: put it behind a TLS-terminating proxy before
//! anyone else uses it (the client refuses `http://` except for this
//! machine). `role` is how the first administrator is made, from the
//! machine the database is on.

use std::process::ExitCode;

fn option(args: &[String], name: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|at| args.get(at + 1))
        .cloned()
        .unwrap_or_else(|| default.to_owned())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db_path = option(&args, "--db", "community.sqlite");
    match args.first().map(String::as_str) {
        Some("serve") => {
            let listen = option(&args, "--listen", "127.0.0.1:8770");
            let db = match eon_stream_community::db::open(&db_path) {
                Ok(db) => db,
                Err(error) => {
                    eprintln!("cannot open {db_path}: {error}");
                    return ExitCode::FAILURE;
                }
            };
            // Signing in with EON Account (madde 46): this server's own origin,
            // and each account server it trusts as ISSUER,KID,PUBLIC-KEY -- the
            // key given here, so checking an assertion needs no request out.
            if let Some(origin) = args
                .iter()
                .position(|a| a == "--origin")
                .and_then(|at| args.get(at + 1))
            {
                let issuers: Vec<_> = args
                    .windows(2)
                    .filter(|w| w[0] == "--trust-account")
                    .filter_map(|w| {
                        let mut parts = w[1].splitn(3, ',');
                        let (issuer, kid, public) = (parts.next()?, parts.next()?, parts.next()?);
                        Some(eon_account_client::assertion::KeySet {
                            issuer: issuer.trim_end_matches('/').to_owned(),
                            keys: vec![eon_account_client::assertion::PublishedKey {
                                kid: kid.to_owned(),
                                alg: "Ed25519".to_owned(),
                                public: public.to_owned(),
                            }],
                        })
                    })
                    .collect();
                println!(
                    "Accepting EON Account from {} account server(s) as {origin}",
                    issuers.len()
                );
                eon_stream_community::api::eon_account::configure(
                    eon_stream_community::api::eon_account::Trust {
                        origin: origin.trim_end_matches('/').to_owned(),
                        issuers,
                    },
                );
            }
            let server = match tiny_http::Server::http(&listen) {
                Ok(server) => server,
                Err(error) => {
                    eprintln!("cannot listen on {listen}: {error}");
                    return ExitCode::FAILURE;
                }
            };
            println!(
                "EON community server {} on http://{listen}, data in {db_path}",
                env!("CARGO_PKG_VERSION")
            );
            println!("Put a TLS proxy in front of it before anyone else connects.");
            eon_stream_community::serve(&server, &db);
            ExitCode::SUCCESS
        }
        Some("role") => {
            let (Some(user), Some(role)) = (args.get(1), args.get(2)) else {
                eprintln!("usage: eon-community role <username> <user|moderator|admin>");
                return ExitCode::FAILURE;
            };
            if !matches!(role.as_str(), "user" | "moderator" | "admin") {
                eprintln!("a role is user, moderator or admin");
                return ExitCode::FAILURE;
            }
            let changed = eon_stream_community::db::open(&db_path).and_then(|db| {
                db.execute(
                    "UPDATE users SET role = ?1 WHERE username = ?2",
                    rusqlite::params![role, user],
                )
            });
            match changed {
                Ok(1) => {
                    println!("{user} is now {role}");
                    ExitCode::SUCCESS
                }
                Ok(_) => {
                    eprintln!("there is no account called {user}");
                    ExitCode::FAILURE
                }
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage:\n  eon-community serve [--listen 127.0.0.1:8770] [--db community.sqlite]\n  eon-community role <username> <user|moderator|admin> [--db community.sqlite]");
            ExitCode::FAILURE
        }
    }
}
