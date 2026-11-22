//! PM OS model gateway (0d): one OpenAI-compatible endpoint in front of the local
//! open-weight models on the Spark.
//!
//! - Callers name a role (`pmos/default`, `pmos/extraction`), never a model, so a model swap
//!   is a config change plus a benchmark run (D11).
//! - Model servers must be local; prompts never leave the Spark (ADM-3). The one exception
//!   is the testing-only cloud upstreams, the Claude Messages API (`anthropic.rs`) and remote
//!   OpenAI-compatible APIs (`remote.rs`), off unless `PMOS_GATEWAY_CLOUD_TEST=1`.
//! - One GPU is shared fairly: interactive before scheduled before personal work, with a cap
//!   per person (MOD-4).
//! - Every call is logged with who, which role, queue time and tokens; never the text.

pub mod anthropic;
pub mod app;
pub mod config;
pub mod remote;
pub mod retry;
pub mod scheduler;
pub mod stub;
pub mod usage;
pub mod workarounds;

pub use app::{router, GatewayConfig, Timeouts};
