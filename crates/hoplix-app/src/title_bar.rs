use iced::{Task, Theme, window};

// Title bar

#[cfg(windows)]
pub fn apply<Message: Send + 'static>(id: window::Id, theme: &Theme) -> Task<Message> {
    let palette = theme.extended_palette();
    let colors = Colors {
        dark: palette.is_dark,
        caption: color_ref(palette.background.weak.color),
        text: color_ref(palette.background.weak.text),
    };
    window::run(id, move |window| set_colors(window, colors)).discard()
}

#[cfg(not(windows))]
pub fn apply<Message: Send + 'static>(_id: window::Id, _theme: &Theme) -> Task<Message> {
    Task::none()
}

// Windows

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct Colors {
    dark: bool,
    caption: u32,
    text: u32,
}

#[cfg(windows)]
fn color_ref(color: iced::Color) -> u32 {
    let [red, green, blue, _] = color.into_rgba8();
    u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn set_colors(window: &dyn window::Window, colors: Colors) {
    use iced::window::raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
    };

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    let dark = i32::from(colors.dark);
    let attributes: [(i32, *const core::ffi::c_void); 3] = [
        (DWMWA_USE_IMMERSIVE_DARK_MODE, (&raw const dark).cast()),
        (DWMWA_CAPTION_COLOR, (&raw const colors.caption).cast()),
        (DWMWA_TEXT_COLOR, (&raw const colors.text).cast()),
    ];
    for (attribute, value) in attributes {
        let Ok(attribute) = u32::try_from(attribute) else {
            continue;
        };
        unsafe {
            DwmSetWindowAttribute(hwnd, attribute, value, 4);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn colors_use_the_windows_byte_order() {
        assert_eq!(
            color_ref(iced::Color::from_rgb8(0x12, 0x34, 0x56)),
            0x0056_3412
        );
    }
}
