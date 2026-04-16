#![allow(dead_code)]

pub mod codex;

#[derive(Debug, Clone, Copy)]
pub struct AdapterCapabilities {
    pub import_known: bool,
    pub read_current_identity: bool,
    pub switch_account: bool,
    pub login: bool,
    pub launch: bool,
    pub resume: bool,
    pub live_usage: bool,
}

pub trait CliAdapter {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> AdapterCapabilities;
}
