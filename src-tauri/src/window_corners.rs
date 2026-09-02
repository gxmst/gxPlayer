//! Native rounded corners for the borderless main window.
//!
//! The window is borderless, so the shell draws no frame of its own. Rounding it in
//! CSS only rounds the web content: the OS window stays a rectangle, and the strip
//! between the CSS curve and the real window edge shows through as a square outline
//! wherever the backdrop differs from the page background.
//!
//! Asking DWM to round the window instead clips the window itself, so there is no
//! second edge to leak. Windows owns the radius, which also keeps the shell
//! consistent with every other window on the desktop.

#[cfg(windows)]
pub fn apply_rounded(window: &tauri::WebviewWindow) {
    use windows::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };

    let hwnd = match window.hwnd() {
        Ok(hwnd) => hwnd,
        Err(error) => {
            eprintln!("GX_WINDOW rounded corners unavailable: {error}");
            return;
        }
    };
    if hwnd.0.is_null() {
        eprintln!("GX_WINDOW rounded corners unavailable: HWND is null");
        return;
    }
    let preference = DWMWCP_ROUND;
    // Fail soft: the attribute is unsupported before Windows 11, where the window
    // simply keeps square corners.
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const preference).cast(),
            size_of_val(&preference) as u32,
        )
    };
    match result {
        Ok(()) => println!("GX_WINDOW rounded corners applied"),
        Err(error) => eprintln!("GX_WINDOW rounded corners unavailable: {error}"),
    }
}

#[cfg(not(windows))]
pub fn apply_rounded(_window: &tauri::WebviewWindow) {}
