#[derive(Debug, Clone)]
pub enum HostArg {
    Int(i64),
    Str(String),
}

/// Host runtime abs.
///
/// will be used in future for:
/// - fs
/// - gui
/// - net
/// - time
pub trait HostRuntime {
    fn call(&mut self, _name: &str, _args: &[HostArg]) -> Result<(), String>;
}

pub struct NullHost;

impl NullHost {
    pub fn new() -> Self {
        Self
    }
}

impl HostRuntime for NullHost {
    fn call(&mut self, name: &str, _args: &[HostArg]) -> Result<(), String> {
        Err(format!("unsupported host call: {}", name))
    }
}
