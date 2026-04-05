//! Host runtime integration (planned: fs, gui, net, time).
#![allow(dead_code)] // public surface for the future host ABI

#[derive(Debug, Clone)]
pub(crate) enum HostArg {
    Int(i64),
    Str(String),
}

pub(crate) trait HostRuntime {
    fn call(&mut self, _name: &str, _args: &[HostArg]) -> Result<(), String>;
}

pub(crate) struct NullHost;

impl NullHost {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl HostRuntime for NullHost {
    fn call(&mut self, name: &str, _args: &[HostArg]) -> Result<(), String> {
        Err(format!("unsupported host call: {}", name))
    }
}
