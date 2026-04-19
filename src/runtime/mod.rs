//! Runtime services shared by the interpreter and backend assumptions.

pub(crate) mod host;
pub(crate) mod io;
pub(crate) mod tape;

#[cfg(feature = "gui")]
pub(crate) mod gui_io;
