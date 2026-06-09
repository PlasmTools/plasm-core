//! TUI style helpers

use super::*;

pub(crate) fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

pub(crate) fn run_title_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Cyan);
    }
    s
}

pub(crate) fn dim_style() -> Style {
    let mut s = Style::default();
    if !no_color() {
        s = s.fg(Color::DarkGray);
    } else {
        s = s.add_modifier(Modifier::DIM);
    }
    s
}

pub(crate) fn err_emphasis_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Red);
    }
    s
}

pub(crate) fn warn_emphasis_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Yellow);
    }
    s
}

pub(crate) fn api_toggle_on_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Green);
    }
    s
}

pub(crate) fn api_toggle_off_style() -> Style {
    let mut s = Style::default();
    if !no_color() {
        s = s.fg(Color::DarkGray);
    } else {
        s = s.add_modifier(Modifier::DIM);
    }
    s
}

pub(crate) fn selected_row_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
    if !no_color() {
        s = s.fg(Color::Black).bg(Color::Yellow);
    }
    s
}

pub(crate) fn catalog_row_display_name(entry_id: &str, label: &str) -> String {
    if label.trim() == entry_id.trim() {
        entry_id.to_string()
    } else {
        format!("{entry_id} — {label}")
    }
}
