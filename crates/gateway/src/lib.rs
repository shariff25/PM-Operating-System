//! PM OS model gateway (0d): one OpenAI-compatible endpoint on PM OS's host in front of the
//! models of the deployment's model source (`PMOS_MODEL_SOURCE`).
//!
//! - Callers name a role (`pmos/default`, `pmos/extraction`), never a model, so a model swap
//!   is a config change plus a benchmark run (D11).
//! - Under `local` (a `class: production` config) the only model servers are the two pinned
//!   local ones on PM OS's host (ADM-3). The testing-only cloud upstreams, the Claude
//!   Messages API (`anthropic.rs`) and remote OpenAI-compatible APIs (`remote.rs`), run only
//!   as `hosted-test` (a `class: test` config, fake data) with `PMOS_GATEWAY_CLOUD_TEST=1`.
//!   `hosted`, one owner-approved hosted model for real data, is not built and is refused.
//! - One GPU is shared fairly: interactive before scheduled before personal work, with a cap
//!   per person (MOD-4).
//! - Every call is logged with who, which role, queue time and tokens; never the text.

pub mod anthropic;
pub mod app;
pub mod config;
pub mod fallback;
pub mod keys;
pub mod remote;
pub mod resolve;
pub mod retry;
pub mod route;
pub mod scheduler;
pub mod stub;
pub mod usage;
pub mod workarounds;

pub use app::{
    router, router_with, GatewayConfig, RouterOptions, Timeouts, READY_CACHE, REMOTE_READY_CACHE,
};
