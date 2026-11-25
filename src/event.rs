use std::path::PathBuf;

/// Everything the main loop waits on.
pub enum AppEvent {
    Input(ratatui::crossterm::event::Event),
    Fs(Vec<PathBuf>),
    Loaded(crate::loader::Loaded),
    SessionRead(crate::tail::SessionRead),
    Message(String),
}
