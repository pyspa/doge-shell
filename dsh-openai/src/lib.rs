mod client;
mod config;

pub use crate::client::{CANCELLED_MESSAGE, ChatGptClient, is_ctrl_c_cancelled};
pub use crate::config::{DEFAULT_BASE_URL, DEFAULT_MODEL, OpenAiConfig};
