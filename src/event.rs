use std::path::PathBuf;

/// Everything the main loop waits on. `Loaded` is added with the loader.
pub enum AppEvent {
    Input(ratatui::crossterm::event::Event),
    Fs(Vec<PathBuf>),
}
