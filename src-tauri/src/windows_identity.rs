#[cfg(windows)]
pub fn initialize() {
    let result = unsafe {
        windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::w!(
            "com.gxplayer.desktop"
        ))
    };
    match result {
        Ok(()) => println!("GX_WINDOWS AppUserModelID=com.gxplayer.desktop"),
        Err(error) => eprintln!("GX_WINDOWS AppUserModelID unavailable: {error}"),
    }
}

#[cfg(not(windows))]
pub fn initialize() {}
