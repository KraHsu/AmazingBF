fn main() {
    // Tauri codegen runs only when the `gui` feature is enabled.
    #[cfg(feature = "gui")]
    tauri_build::build();
}
