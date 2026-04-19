//! Host runtime integration (planned: fs, gui, net, time).
#![allow(dead_code)] // reason: public surface reserved for the future host ABI; variants used once host-call lowering lands

/// Typed argument passed to a [`HostRuntime`] call.
#[derive(Debug, Clone)]
pub(crate) enum HostArg {
    /// 64-bit signed integer argument.
    Int(i64),
    /// UTF-8 string argument, owned.
    Str(String),
}

/// Abstract host ABI invoked by future host-call HIR lowering.
pub(crate) trait HostRuntime {
    /// Dispatch a host call by `name` with typed positional arguments.
    fn call(&mut self, _name: &str, _args: &[HostArg]) -> Result<(), String>;
}

/// [`HostRuntime`] implementation that rejects every call as unsupported.
pub(crate) struct NullHost;

impl NullHost {
    /// Build a placeholder host that always returns an "unsupported" error.
    pub(crate) fn new() -> Self {
        Self
    }
}

impl HostRuntime for NullHost {
    fn call(&mut self, name: &str, _args: &[HostArg]) -> Result<(), String> {
        Err(format!("unsupported host call: {}", name))
    }
}
