//! The Bots tool port (`server/tools/bots/`): the load tool that drives
//! a swarm of scripted client bots against a live server.
//!
//! The reference is one binary (`BIGWORLD_MAIN` -> `bwMainT< MainApp >`);
//! the port grows the same surface as a library the tool binary wraps --
//! the config face first ([`config`]), then the batch add/remove, the
//! logout events and the script-driven movement faces.
//!
//! - `config` -- `BotsConfig` (`bots_config.{hpp,cpp}`): the
//!   `bots/`-prefixed option set read from the bw.xml chain, the
//!   derived transport and the boot-time `bots/shouldLog` read;
//! - `client_app` -- the `ClientApp` entry face
//!   (`client_app.{hpp,cpp}`): one simulated client in the active set,
//!   its logout events and its movement walk;
//! - `movement` -- the controller interface, the factory registry and
//!   the "Beeline" controller (`movement_controller.*`,
//!   `beeline_controller.*`);
//! - `patrol_graph` -- the patrol node graph, its cache and the
//!   "Patrol" controller (`patrol_graph.*`);
//! - `zigzag_patrol_graph` -- the corridor zigzag walker and the
//!   "ZigzagPatrol" controller (`zigzag_patrol_graph.*`);
//! - `main_app` -- `MainApp` (`main_app.{hpp,cpp}`): the active set,
//!   the batch add/remove commands, the tick slice, the retry queue
//!   and the controller factory.

pub mod client_app;
pub mod config;
pub mod main_app;
pub mod movement;
pub mod patrol_graph;
pub mod zigzag_patrol_graph;
