//! Trading bot.
//!
//! Two components share this library:
//! - the CLI, which manages configuration and the cron job
//! - the cron job, a single crontab line that invokes `stonks run`

pub mod auth;
pub mod bot;
pub mod broker;
pub mod chatgpt;
pub mod cli;
pub mod config;
pub mod cron;
pub mod llm;
pub mod models;
pub mod providers;
