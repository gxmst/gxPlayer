#[cfg(windows)]
mod imp {
    use std::cmp::min;
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use gx_audio::engine::LocalAudioEngine;
    use gx_contracts::PlaybackStatus;
    use tauri::{AppHandle, Manager};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject, HBITMAP, HGDIOBJ,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Shell::{
        DefSubclassProc, GetWindowSubclass, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass,
        THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED, THBN_CLICKED, THUMBBUTTON,
        TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, RegisterWindowMessageW,
        SM_CXICON, SM_CYICON, WM_COMMAND, WM_NCDESTROY,
    };
    use windows::core::w;

    use crate::transport::{TransportAction, TransportState, dispatch};

    const MONITOR_INTERVAL: Duration = Duration::from_millis(200);
    const SUBCLASS_ID: usize = 0x4758_5442;
    const BUTTON_PREVIOUS: u32 = 0x4751;
    const BUTTON_PLAY_PAUSE: u32 = 0x4752;
    const BUTTON_NEXT: u32 = 0x4753;
    const GLYPH_COLOR: u32 = 0xff20_2020;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PlaybackVisual {
        Play,
        Pause,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ToolbarState {
        revision: u64,
        playback: PlaybackVisual,
        has_current: bool,
        can_previous: bool,
        can_next: bool,
    }

    impl ToolbarState {
        fn unavailable() -> Self {
            Self {
                revision: 0,
                playback: PlaybackVisual::Play,
                has_current: false,
                can_previous: false,
                can_next: false,
            }
        }

        fn button_state_eq(self, other: Self) -> bool {
            self.playback == other.playback
                && self.has_current == other.has_current
                && self.can_previous == other.can_previous
                && self.can_next == other.can_next
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ClickedButton {
        Previous,
        PlayPause,
        Next,
    }

    #[derive(Debug, Clone, Copy)]
    enum Glyph {
        Previous,
        Play,
        Pause,
        Next,
    }

    struct ToolbarIcons {
        previous: OwnedIcon,
        play: OwnedIcon,
        pause: OwnedIcon,
        next: OwnedIcon,
    }

    impl ToolbarIcons {
        fn create() -> Result<Self, String> {
            let width = system_icon_dimension(SM_CXICON);
            let height = system_icon_dimension(SM_CYICON);
            Ok(Self {
                previous: OwnedIcon(create_glyph_icon(Glyph::Previous, width, height)?),
                play: OwnedIcon(create_glyph_icon(Glyph::Play, width, height)?),
                pause: OwnedIcon(create_glyph_icon(Glyph::Pause, width, height)?),
                next: OwnedIcon(create_glyph_icon(Glyph::Next, width, height)?),
            })
        }
    }

    struct OwnedIcon(HICON);

    impl OwnedIcon {
        fn handle(&self) -> HICON {
            self.0
        }
    }

    impl Drop for OwnedIcon {
        fn drop(&mut self) {
            if !self.0.is_invalid()
                && let Err(error) = unsafe { DestroyIcon(self.0) }
            {
                eprintln!("GX_TASKBAR DestroyIcon failed: {error}");
            }
        }
    }

    struct BitmapGuard(HBITMAP);

    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
            }
        }
    }

    struct ToolbarContext {
        app: AppHandle,
        taskbar_button_created: u32,
        taskbar: Option<ITaskbarList3>,
        buttons_ready: bool,
        state: ToolbarState,
        icons: ToolbarIcons,
        alive: Arc<AtomicBool>,
    }

    impl Drop for ToolbarContext {
        fn drop(&mut self) {
            self.alive.store(false, Ordering::Release);
        }
    }

    pub fn install(app: AppHandle) -> Result<(), String> {
        let raw_hwnd = crate::media_session::main_hwnd(&app)?;
        if raw_hwnd.is_null() {
            return Err("main HWND is null".into());
        }
        let hwnd = HWND(raw_hwnd);

        let already_installed =
            unsafe { GetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, None).as_bool() };
        if already_installed {
            return Ok(());
        }

        let taskbar_button_created = unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) };
        if taskbar_button_created == 0 {
            return Err("RegisterWindowMessageW(TaskbarButtonCreated) returned 0".into());
        }

        let icons = ToolbarIcons::create()?;
        let initial_state = read_toolbar_state(&app).unwrap_or_else(ToolbarState::unavailable);
        let alive = Arc::new(AtomicBool::new(true));
        let context = Box::new(ToolbarContext {
            app: app.clone(),
            taskbar_button_created,
            taskbar: None,
            buttons_ready: false,
            state: initial_state,
            icons,
            alive: Arc::clone(&alive),
        });

        spawn_status_monitor(app, raw_hwnd as usize, Arc::clone(&alive), initial_state)?;

        let context_ptr = Box::into_raw(context);
        let installed = unsafe {
            SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, context_ptr as usize)
                .as_bool()
        };
        if !installed {
            alive.store(false, Ordering::Release);
            unsafe { drop(Box::from_raw(context_ptr)) };
            return Err("SetWindowSubclass failed".into());
        }

        println!("GX_TASKBAR toolbar subclass installed hwnd={hwnd:?}");
        Ok(())
    }

    fn spawn_status_monitor(
        app: AppHandle,
        hwnd: usize,
        alive: Arc<AtomicBool>,
        initial_state: ToolbarState,
    ) -> Result<(), String> {
        thread::Builder::new()
            .name("gx-taskbar-toolbar".into())
            .spawn(move || {
                let mut last_state = initial_state;
                while alive.load(Ordering::Acquire) {
                    if let Some(state) = read_toolbar_state(&app)
                        && state != last_state
                    {
                        last_state = state;
                        let scheduled = app.run_on_main_thread(move || {
                            update_toolbar_state_on_main(HWND(hwnd as *mut c_void), state);
                        });
                        if let Err(error) = scheduled {
                            eprintln!("GX_TASKBAR main-thread update failed: {error}");
                            break;
                        }
                    }
                    thread::sleep(MONITOR_INTERVAL);
                }
            })
            .map(|_| ())
            .map_err(|error| format!("failed to spawn taskbar status monitor: {error}"))
    }

    fn read_toolbar_state(app: &AppHandle) -> Option<ToolbarState> {
        let engine = app.try_state::<LocalAudioEngine>()?;
        let transport = app.try_state::<TransportState>()?.snapshot();
        Some(ToolbarState {
            revision: transport.revision,
            playback: playback_visual(engine.snapshot().status),
            has_current: transport.has_current,
            can_previous: transport.can_previous,
            can_next: transport.can_next,
        })
    }

    fn playback_visual(status: PlaybackStatus) -> PlaybackVisual {
        if matches!(status, PlaybackStatus::Playing | PlaybackStatus::Loading) {
            PlaybackVisual::Pause
        } else {
            PlaybackVisual::Play
        }
    }

    fn update_toolbar_state_on_main(hwnd: HWND, state: ToolbarState) {
        let mut reference_data = 0usize;
        let found = unsafe {
            GetWindowSubclass(
                hwnd,
                Some(subclass_proc),
                SUBCLASS_ID,
                Some(&mut reference_data),
            )
            .as_bool()
        };
        if !found || reference_data == 0 {
            return;
        }

        let context = unsafe { &mut *(reference_data as *mut ToolbarContext) };
        let buttons_changed = !context.state.button_state_eq(state);
        context.state = state;
        if !buttons_changed || !context.buttons_ready {
            return;
        }

        let Some(taskbar) = context.taskbar.as_ref() else {
            context.buttons_ready = false;
            return;
        };
        let buttons = toolbar_buttons(&context.icons, state);
        if let Err(error) = unsafe { taskbar.ThumbBarUpdateButtons(hwnd, &buttons) } {
            eprintln!("GX_TASKBAR ThumbBarUpdateButtons failed: {error}");
            context.buttons_ready = false;
            context.taskbar = None;
        }
    }

    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _subclass_id: usize,
        reference_data: usize,
    ) -> LRESULT {
        match catch_unwind(AssertUnwindSafe(|| {
            subclass_proc_inner(hwnd, message, wparam, lparam, reference_data)
        })) {
            Ok(result) => result,
            Err(_) => {
                eprintln!("GX_TASKBAR panic contained in window subclass");
                if message == WM_NCDESTROY {
                    cleanup_subclass(hwnd, message, wparam, lparam, reference_data)
                } else {
                    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
                }
            }
        }
    }

    fn subclass_proc_inner(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        reference_data: usize,
    ) -> LRESULT {
        if message == WM_NCDESTROY {
            return cleanup_subclass(hwnd, message, wparam, lparam, reference_data);
        }
        if reference_data == 0 {
            return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        }

        let context = unsafe { &mut *(reference_data as *mut ToolbarContext) };
        if message == context.taskbar_button_created {
            if let Err(error) = add_toolbar_buttons(hwnd, context) {
                eprintln!("GX_TASKBAR add buttons failed: {error}");
            }
            return LRESULT(0);
        }

        if message == WM_COMMAND
            && let Some(button) = decode_thumbbar_click(wparam.0)
        {
            let action = match button {
                ClickedButton::Previous => TransportAction::Previous,
                ClickedButton::PlayPause => TransportAction::Toggle,
                ClickedButton::Next => TransportAction::Next,
            };
            println!("GX_TASKBAR received click: {button:?}");
            let app = context.app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = dispatch(&app, action) {
                    eprintln!("GX_TASKBAR command failed: {error}");
                }
            });
            return LRESULT(0);
        }

        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    fn cleanup_subclass(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        reference_data: usize,
    ) -> LRESULT {
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
        let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        if reference_data != 0 {
            unsafe { drop(Box::from_raw(reference_data as *mut ToolbarContext)) };
        }
        result
    }

    fn add_toolbar_buttons(hwnd: HWND, context: &mut ToolbarContext) -> Result<(), String> {
        context.buttons_ready = false;
        context.taskbar = None;

        let taskbar: ITaskbarList3 = unsafe {
            CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| format!("CoCreateInstance(TaskbarList): {error}"))?
        };
        unsafe { taskbar.HrInit() }.map_err(|error| format!("ITaskbarList3::HrInit: {error}"))?;

        let buttons = toolbar_buttons(&context.icons, context.state);
        unsafe { taskbar.ThumbBarAddButtons(hwnd, &buttons) }
            .map_err(|error| format!("ThumbBarAddButtons: {error}"))?;

        context.taskbar = Some(taskbar);
        context.buttons_ready = true;
        println!("GX_TASKBAR thumbnail buttons attached hwnd={hwnd:?}");
        Ok(())
    }

    fn decode_thumbbar_click(wparam: usize) -> Option<ClickedButton> {
        let notification = ((wparam >> 16) & 0xffff) as u32;
        if notification != THBN_CLICKED {
            return None;
        }
        match (wparam & 0xffff) as u32 {
            BUTTON_PREVIOUS => Some(ClickedButton::Previous),
            BUTTON_PLAY_PAUSE => Some(ClickedButton::PlayPause),
            BUTTON_NEXT => Some(ClickedButton::Next),
            _ => None,
        }
    }

    fn toolbar_buttons(icons: &ToolbarIcons, state: ToolbarState) -> [THUMBBUTTON; 3] {
        let play_pause = match state.playback {
            PlaybackVisual::Play => (icons.play.handle(), "播放"),
            PlaybackVisual::Pause => (icons.pause.handle(), "暂停"),
        };
        [
            thumb_button(
                BUTTON_PREVIOUS,
                icons.previous.handle(),
                "上一首",
                state.has_current && state.can_previous,
            ),
            thumb_button(
                BUTTON_PLAY_PAUSE,
                play_pause.0,
                play_pause.1,
                state.has_current,
            ),
            thumb_button(
                BUTTON_NEXT,
                icons.next.handle(),
                "下一首",
                state.has_current && state.can_next,
            ),
        ]
    }

    fn thumb_button(id: u32, icon: HICON, tooltip: &str, enabled: bool) -> THUMBBUTTON {
        THUMBBUTTON {
            dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
            iId: id,
            hIcon: icon,
            szTip: tooltip_utf16(tooltip),
            dwFlags: if enabled { THBF_ENABLED } else { THBF_DISABLED },
            ..THUMBBUTTON::default()
        }
    }

    fn tooltip_utf16(value: &str) -> [u16; 260] {
        let mut output = [0u16; 260];
        for (slot, value) in output.iter_mut().take(259).zip(value.encode_utf16()) {
            *slot = value;
        }
        output
    }

    fn system_icon_dimension(
        metric: windows::Win32::UI::WindowsAndMessaging::SYSTEM_METRICS_INDEX,
    ) -> i32 {
        let value = unsafe { GetSystemMetrics(metric) };
        if value > 0 { value.clamp(16, 128) } else { 32 }
    }

    fn create_glyph_icon(glyph: Glyph, width: i32, height: i32) -> Result<HICON, String> {
        let width_usize = usize::try_from(width).map_err(|_| "invalid icon width")?;
        let height_usize = usize::try_from(height).map_err(|_| "invalid icon height")?;
        let pixel_count = width_usize
            .checked_mul(height_usize)
            .ok_or_else(|| "icon dimensions overflow".to_owned())?;
        let image_size = pixel_count
            .checked_mul(size_of::<u32>())
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| "icon image size overflow".to_owned())?;

        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: image_size,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };

        let mut color_bits = std::ptr::null_mut();
        let color_bitmap = unsafe {
            CreateDIBSection(None, &bitmap_info, DIB_RGB_COLORS, &mut color_bits, None, 0)
                .map_err(|error| format!("CreateDIBSection: {error}"))?
        };
        let color_bitmap = BitmapGuard(color_bitmap);
        if color_bits.is_null() {
            return Err("CreateDIBSection returned null pixel data".into());
        }

        let pixels =
            unsafe { std::slice::from_raw_parts_mut(color_bits.cast::<u32>(), pixel_count) };
        pixels.fill(0);
        paint_glyph(pixels, width_usize, height_usize, glyph);

        let mask_stride = width_usize.div_ceil(16) * 2;
        let mut mask_bits = vec![0xffu8; mask_stride * height_usize];
        for y in 0..height_usize {
            for x in 0..width_usize {
                if pixels[y * width_usize + x] != 0 {
                    let byte = y * mask_stride + x / 8;
                    mask_bits[byte] &= !(0x80 >> (x % 8));
                }
            }
        }
        let mask_bitmap = unsafe {
            CreateBitmap(
                width,
                height,
                1,
                1,
                Some(mask_bits.as_ptr().cast::<c_void>()),
            )
        };
        if mask_bitmap.is_invalid() {
            return Err("CreateBitmap(mask) failed".into());
        }
        let mask_bitmap = BitmapGuard(mask_bitmap);

        let icon_info = ICONINFO {
            fIcon: true.into(),
            hbmMask: mask_bitmap.0,
            hbmColor: color_bitmap.0,
            ..ICONINFO::default()
        };
        unsafe { CreateIconIndirect(&icon_info) }
            .map_err(|error| format!("CreateIconIndirect: {error}"))
    }

    fn paint_glyph(pixels: &mut [u32], width: usize, height: usize, glyph: Glyph) {
        let side = min(width, height);
        let origin_x = (width - side) / 2;
        let origin_y = (height - side) / 2;
        let padding = (side / 5).max(2);
        let left = origin_x + padding;
        let right = origin_x + side - padding - 1;
        let top = origin_y + padding;
        let bottom = origin_y + side - padding;
        let stroke = (side / 8).max(2);
        let gap = (side / 12).max(1);

        match glyph {
            Glyph::Play => paint_right_triangle(pixels, width, left, right, top, bottom),
            Glyph::Pause => {
                let bar_width = (side / 7).max(2);
                let bar_gap = (side / 7).max(2);
                let total = bar_width * 2 + bar_gap;
                let start = origin_x + (side - total) / 2;
                fill_rect(pixels, width, start, top, start + bar_width, bottom);
                fill_rect(
                    pixels,
                    width,
                    start + bar_width + bar_gap,
                    top,
                    start + total,
                    bottom,
                );
            }
            Glyph::Previous => {
                fill_rect(pixels, width, left, top, left + stroke, bottom);
                paint_left_triangle(pixels, width, left + stroke + gap, right, top, bottom);
            }
            Glyph::Next => {
                fill_rect(pixels, width, right - stroke + 1, top, right + 1, bottom);
                paint_right_triangle(pixels, width, left, right - stroke - gap, top, bottom);
            }
        }
    }

    fn paint_right_triangle(
        pixels: &mut [u32],
        width: usize,
        base_x: usize,
        tip_x: usize,
        top: usize,
        bottom: usize,
    ) {
        if bottom <= top || tip_x <= base_x {
            return;
        }
        let center = (top + bottom - 1) / 2;
        let half_height = (bottom - top).div_ceil(2).max(1);
        for y in top..bottom {
            let distance = y.abs_diff(center).min(half_height);
            let reach = (half_height - distance) * (tip_x - base_x) / half_height;
            fill_rect(pixels, width, base_x, y, base_x + reach + 1, y + 1);
        }
    }

    fn paint_left_triangle(
        pixels: &mut [u32],
        width: usize,
        tip_x: usize,
        base_x: usize,
        top: usize,
        bottom: usize,
    ) {
        if bottom <= top || base_x <= tip_x {
            return;
        }
        let center = (top + bottom - 1) / 2;
        let half_height = (bottom - top).div_ceil(2).max(1);
        for y in top..bottom {
            let distance = y.abs_diff(center).min(half_height);
            let reach = (half_height - distance) * (base_x - tip_x) / half_height;
            fill_rect(pixels, width, base_x - reach, y, base_x + 1, y + 1);
        }
    }

    fn fill_rect(
        pixels: &mut [u32],
        width: usize,
        left: usize,
        top: usize,
        right: usize,
        bottom: usize,
    ) {
        let height = pixels.len() / width;
        for y in top.min(height)..bottom.min(height) {
            for x in left.min(width)..right.min(width) {
                pixels[y * width + x] = GLYPH_COLOR;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn decodes_only_thumbbar_clicks_for_known_ids() {
            let click = ((THBN_CLICKED as usize) << 16) | BUTTON_PLAY_PAUSE as usize;
            assert_eq!(decode_thumbbar_click(click), Some(ClickedButton::PlayPause));
            assert_eq!(decode_thumbbar_click(BUTTON_PLAY_PAUSE as usize), None);
            let unknown = ((THBN_CLICKED as usize) << 16) | 0xffff;
            assert_eq!(decode_thumbbar_click(unknown), None);
        }

        #[test]
        fn tooltip_is_truncated_and_null_terminated() {
            let value = "曲".repeat(300);
            let encoded = tooltip_utf16(&value);
            assert!(encoded[..259].iter().all(|value| *value != 0));
            assert_eq!(encoded[259], 0);
        }

        #[test]
        fn playback_visual_matches_frontend_transport_semantics() {
            assert_eq!(
                playback_visual(PlaybackStatus::Playing),
                PlaybackVisual::Pause
            );
            assert_eq!(
                playback_visual(PlaybackStatus::Loading),
                PlaybackVisual::Pause
            );
            assert_eq!(
                playback_visual(PlaybackStatus::Paused),
                PlaybackVisual::Play
            );
            assert_eq!(
                playback_visual(PlaybackStatus::Buffering),
                PlaybackVisual::Play
            );
        }

        #[test]
        fn revision_does_not_force_a_button_update() {
            let first = ToolbarState {
                revision: 1,
                playback: PlaybackVisual::Play,
                has_current: true,
                can_previous: false,
                can_next: true,
            };
            let second = ToolbarState {
                revision: 2,
                ..first
            };
            assert_ne!(first, second);
            assert!(first.button_state_eq(second));
        }

        #[test]
        fn generated_glyphs_use_opaque_dark_pixels_on_transparent_backgrounds() {
            for glyph in [Glyph::Previous, Glyph::Play, Glyph::Pause, Glyph::Next] {
                let mut pixels = vec![0; 32 * 32];
                paint_glyph(&mut pixels, 32, 32, glyph);

                assert!(pixels.contains(&GLYPH_COLOR));
                assert!(pixels.contains(&0));
                assert!(pixels.iter().all(|pixel| matches!(*pixel, 0 | GLYPH_COLOR)));
                assert_eq!(GLYPH_COLOR >> 24, 0xff);
                assert_eq!(GLYPH_COLOR & 0x00ff_ffff, 0x0020_2020);
            }
        }

        #[test]
        fn generated_toolbar_glyph_shapes_remain_distinct() {
            fn render(glyph: Glyph) -> Vec<u32> {
                let mut pixels = vec![0; 32 * 32];
                paint_glyph(&mut pixels, 32, 32, glyph);
                pixels
            }

            let previous = render(Glyph::Previous);
            let play = render(Glyph::Play);
            let pause = render(Glyph::Pause);
            let next = render(Glyph::Next);

            assert_ne!(previous, play);
            assert_ne!(previous, next);
            assert_ne!(play, pause);
            assert_ne!(play, next);
            assert_ne!(pause, next);
        }
    }
}

#[cfg(windows)]
pub use imp::install;

#[cfg(not(windows))]
pub fn install(_app: tauri::AppHandle) -> Result<(), String> {
    Ok(())
}
