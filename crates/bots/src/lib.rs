//! The Bots tool port (`server/tools/bots/`): the load tool that drives
//! a swarm of scripted client bots against a live server.
//!
//! The reference is one binary (`BIGWORLD_MAIN` -> `bwMainT< MainApp >`);
//! the port grows the same surface as a library the tool binary wraps --
//! the config face first ([`config`]), then the batch add/remove and the
//! script-driven faces.
//!
//! - `config` -- `BotsConfig` (`bots_config.{hpp,cpp}`): the
//!   `bots/`-prefixed option set read from the bw.xml chain, the
//!   derived transport and the boot-time `bots/shouldLog` read.

pub mod config;
