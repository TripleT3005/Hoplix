use iced::theme::{self, Mode};
use iced::widget::{button, container};
use iced::{Background, Border, Color, Theme, color};

use crate::compare::Mark;

// Types

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub name: &'static str,
    pub background: Color,
    pub text: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
}

// Constants

const TINT_ALPHA: f32 = 0.16;
const MARK_ALPHA: f32 = 0.08;
const MARK_TEXT_MIX: f32 = 0.25;

// Palettes

pub const LIGHT: Palette = Palette {
    name: "Hoplix Light",
    background: color!(0xF7F7F9),
    text: color!(0x1A1C22),
    accent: color!(0x4355DB),
    success: color!(0x1F7A4A),
    warning: color!(0x9A5B00),
    danger: color!(0xC0262D),
    info: color!(0x2F5FB3),
};

pub const DARK: Palette = Palette {
    name: "Hoplix Dark",
    background: color!(0x16181D),
    text: color!(0xE6E8EE),
    accent: color!(0x4355DB),
    success: color!(0x4CC38A),
    warning: color!(0xE6B450),
    danger: color!(0xF2726B),
    info: color!(0x7AA2F7),
};

// Theme

pub fn palette(mode: Mode) -> &'static Palette {
    match mode {
        Mode::Dark => &DARK,
        Mode::Light | Mode::None => &LIGHT,
    }
}

pub fn app_theme(mode: Mode) -> Theme {
    let palette = palette(mode);
    Theme::custom(
        palette.name,
        theme::Palette {
            background: palette.background,
            text: palette.text,
            primary: palette.accent,
            success: palette.success,
            warning: palette.warning,
            danger: palette.danger,
        },
    )
}

pub fn mode_override() -> Option<Mode> {
    match std::env::var("ICED_THEME").ok()?.as_str() {
        "Light" => Some(Mode::Light),
        "Dark" => Some(Mode::Dark),
        _ => None,
    }
}

// Styles

pub fn bar(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        ..container::Style::default()
    }
}

pub fn pane(theme: &Theme, focused: bool, dropping: bool) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(if dropping {
            palette.primary.weak.color.scale_alpha(0.25)
        } else {
            palette.background.base.color
        })),
        border: Border {
            color: if focused || dropping {
                palette.primary.base.color
            } else {
                palette.background.strong.color
            },
            width: if dropping { 2.0 } else { 1.0 },
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

pub fn row(theme: &Theme, selected: bool, even: bool) -> container::Style {
    let palette = theme.extended_palette();
    let background = if selected {
        Some(palette.primary.weak.color)
    } else if even {
        None
    } else {
        Some(Color {
            a: 0.35,
            ..palette.background.weak.color
        })
    };
    container::Style {
        background: background.map(Background::Color),
        text_color: selected.then_some(palette.primary.weak.text),
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

pub fn mark_color(theme: &Theme, mark: Mark) -> Color {
    let palette = if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
    };
    let color = match mark {
        Mark::OnlyHere => palette.success,
        Mark::NewerHere | Mark::ContentDiffers => palette.warning,
        Mark::OlderHere => palette.info,
        Mark::SizeDiffers | Mark::KindDiffers => palette.danger,
        Mark::Equal => return palette.text,
    };
    blend(palette.text.scale_alpha(MARK_TEXT_MIX), color)
}

pub fn compared_row(theme: &Theme, selected: bool, even: bool, mark: Mark) -> container::Style {
    let style = row(theme, selected, even);
    if selected || mark == Mark::Equal {
        return style;
    }
    let base = theme.extended_palette().background.base.color;
    let tint = mark_color(theme, mark).scale_alpha(MARK_ALPHA);
    let under = match style.background {
        Some(Background::Color(color)) => blend(color, base),
        _ => base,
    };
    container::Style {
        background: Some(Background::Color(blend(tint, under))),
        ..style
    }
}

fn blend(top: Color, bottom: Color) -> Color {
    let mix = |a: f32, b: f32| a * top.a + b * (1.0 - top.a);
    Color::from_rgb(
        mix(top.r, bottom.r),
        mix(top.g, bottom.g),
        mix(top.b, bottom.b),
    )
}

pub fn notice(theme: &Theme) -> container::Style {
    tinted(theme, theme.extended_palette().danger.base.color)
}

pub fn warning(theme: &Theme) -> container::Style {
    tinted(theme, theme.extended_palette().warning.base.color)
}

fn tinted(theme: &Theme, color: Color) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(color.scale_alpha(TINT_ALPHA))),
        text_color: Some(palette.background.base.text),
        border: Border {
            color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

pub fn secondary_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let base = button::Style {
        background: Some(Background::Color(palette.background.strong.color)),
        text_color: palette.background.strong.text,
        border: Border {
            radius: 2.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    };
    match status {
        button::Status::Active | button::Status::Pressed => base,
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(palette.background.stronger.color)),
            ..base
        },
        button::Status::Disabled => disabled(base),
    }
}

pub fn danger_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let text_color = if palette.is_dark {
        palette.background.base.color
    } else {
        Color::WHITE
    };
    let style = button::Style {
        text_color,
        ..button::danger(theme, button::Status::Active)
    };
    match status {
        button::Status::Disabled => disabled(style),
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(palette.danger.strong.color)),
            ..style
        },
        button::Status::Active | button::Status::Pressed => style,
    }
}

fn disabled(style: button::Style) -> button::Style {
    button::Style {
        background: style
            .background
            .map(|background| background.scale_alpha(0.5)),
        text_color: style.text_color.scale_alpha(0.5),
        ..style
    }
}

pub fn tab(theme: &Theme, status: button::Status, active: bool) -> button::Style {
    let palette = theme.extended_palette();
    let background = match (active, status) {
        (true, _) => Some(palette.background.base.color),
        (false, button::Status::Hovered | button::Status::Pressed) => {
            Some(palette.background.strong.color)
        }
        (false, _) => None,
    };
    button::Style {
        background: background.map(Background::Color),
        text_color: if active {
            palette.background.base.text
        } else {
            palette.background.weak.text
        },
        border: Border {
            color: if active {
                palette.primary.base.color
            } else {
                Color::TRANSPARENT
            },
            width: if active { 1.0 } else { 0.0 },
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color) -> f32 {
        let channel = |value: f32| {
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        let (light, dark) = if a > b { (a, b) } else { (b, a) };
        (light + 0.05) / (dark + 0.05)
    }

    // Contrast

    #[test]
    fn text_is_readable_on_the_background() {
        for palette in [&LIGHT, &DARK] {
            assert!(
                contrast(palette.text, palette.background) >= 7.0,
                "{}",
                palette.name
            );
        }
    }

    #[test]
    fn white_text_is_readable_on_the_accent() {
        for palette in [&LIGHT, &DARK] {
            assert!(
                contrast(Color::WHITE, palette.accent) >= 4.5,
                "{}",
                palette.name
            );
        }
    }

    #[test]
    fn status_colors_are_readable_on_the_background() {
        for palette in [&LIGHT, &DARK] {
            for color in [palette.success, palette.warning, palette.danger] {
                assert!(
                    contrast(color, palette.background) >= 4.5,
                    "{} {color:?}",
                    palette.name
                );
            }
        }
    }

    fn over(color: Color, background: Color) -> Color {
        let mix = |top: f32, bottom: f32| top * color.a + bottom * (1.0 - color.a);
        Color::from_rgb(
            mix(color.r, background.r),
            mix(color.g, background.g),
            mix(color.b, background.b),
        )
    }

    fn fill(background: Option<Background>) -> Color {
        match background {
            Some(Background::Color(color)) => color,
            _ => panic!("expected a solid background"),
        }
    }

    #[test]
    fn buttons_are_readable_and_differ_from_disabled_ones() {
        for mode in [Mode::Light, Mode::Dark] {
            let theme = app_theme(mode);
            let base = palette(mode).background;
            for style in [secondary_button, danger_button] {
                let active = style(&theme, button::Status::Active);
                let background = over(fill(active.background), base);
                assert!(contrast(active.text_color, background) >= 4.5, "{mode:?}");
                let off = style(&theme, button::Status::Disabled);
                let text = over(off.text_color, over(fill(off.background), base));
                assert!(contrast(text, background) > 1.5, "{mode:?}");
            }
        }
    }

    #[test]
    fn comparison_marks_are_readable_on_marked_rows() {
        let marks = [
            Mark::OnlyHere,
            Mark::NewerHere,
            Mark::OlderHere,
            Mark::SizeDiffers,
            Mark::KindDiffers,
            Mark::ContentDiffers,
        ];
        for mode in [Mode::Light, Mode::Dark] {
            let theme = app_theme(mode);
            let text = palette(mode).text;
            for mark in marks {
                for even in [true, false] {
                    let background = fill(compared_row(&theme, false, even, mark).background);
                    let color = mark_color(&theme, mark);
                    assert!(contrast(color, background) >= 4.5, "{mode:?} {mark:?}");
                    assert!(contrast(text, background) >= 7.0, "{mode:?} {mark:?}");
                }
            }
            let distinct: Vec<Color> = marks.iter().map(|mark| mark_color(&theme, *mark)).collect();
            assert_ne!(distinct[0], distinct[1]);
            assert_ne!(distinct[1], distinct[2]);
            assert_ne!(distinct[2], distinct[3]);
        }
    }

    #[test]
    fn notices_are_readable() {
        for mode in [Mode::Light, Mode::Dark] {
            let theme = app_theme(mode);
            let base = palette(mode).background;
            for style in [notice(&theme), warning(&theme)] {
                let background = over(fill(style.background), base);
                let text = style.text_color.expect("notices set a text color");
                assert!(contrast(text, background) >= 7.0, "{mode:?}");
            }
        }
    }

    // Selection

    #[test]
    fn mode_selects_the_palette() {
        assert_eq!(palette(Mode::Dark).name, "Hoplix Dark");
        assert_eq!(palette(Mode::Light).name, "Hoplix Light");
        assert_eq!(palette(Mode::None).name, "Hoplix Light");
        assert!(app_theme(Mode::Dark).extended_palette().is_dark);
        assert!(!app_theme(Mode::Light).extended_palette().is_dark);
    }
}
