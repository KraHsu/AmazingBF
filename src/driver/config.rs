#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub source: String,
    pub mode: RunMode
}

#[derive(Debug, Clone, Copy)]
pub enum RunMode {
    /// output fontend and IR
    Dump,
    Interpret,
    ToElf,
}
