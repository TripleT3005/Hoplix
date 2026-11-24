#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod compare;
mod connect_form;
mod edits;
mod format;
mod i18n;
mod icon;
mod local_time;
mod opener;
mod session;
mod settings;
mod site_actions;
mod site_form;
mod theme;
mod title_bar;
mod transfers;
mod ui;

#[cfg(test)]
mod ui_tests;

use app::Hoplix;
use session::Session;

// Constants

const MIN_WINDOW_SIZE: iced::Size = iced::Size::new(640.0, 400.0);
const WINDOW_ICON_SIZE: u8 = 64;

// Entry point

fn main() -> iced::Result {
    let window = hoplix_core::config_dir()
        .map(|directory| Session::load(&directory))
        .and_then(|session| session.window)
        .unwrap_or_default();
    let position = window
        .usable_position()
        .map_or(iced::window::Position::Default, |(x, y)| {
            iced::window::Position::Specific(iced::Point::new(x, y))
        });
    iced::application(Hoplix::new, Hoplix::update, Hoplix::view)
        .title(Hoplix::title)
        .theme(Hoplix::theme)
        .subscription(Hoplix::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(window.width, window.height),
            position,
            maximized: window.maximized,
            min_size: Some(MIN_WINDOW_SIZE),
            icon: icon::window_icon(WINDOW_ICON_SIZE),
            exit_on_close_request: false,
            ..iced::window::Settings::default()
        })
        .run()
}
