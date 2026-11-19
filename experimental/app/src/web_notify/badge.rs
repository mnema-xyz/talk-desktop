//! The taskbar/dock badge: the sum of the mentions over all servers.

/// The icon badge for the sum over all servers.
#[cfg(not(windows))]
pub(crate) fn apply_badge(window: &tauri::Window, count: u32) -> Result<(), String> {
    window
        .set_badge_count((count > 0).then_some(i64::from(count)))
        .map_err(|_| "The badge could not be set.".into())
}

/// Windows has no counter on the taskbar button, only an overlay icon: a red dot.
#[cfg(windows)]
pub(crate) fn apply_badge(window: &tauri::Window, count: u32) -> Result<(), String> {
    let icon = (count > 0).then(dot);
    window
        .set_overlay_icon(icon)
        .map_err(|_| "The badge could not be set.".into())
}

#[cfg(windows)]
fn dot() -> tauri::image::Image<'static> {
    const SIZE: u32 = 16;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
            let inside = dx * dx + dy * dy <= 7.0 * 7.0;
            rgba.extend_from_slice(if inside {
                &[0xe0, 0x32, 0x3c, 0xff]
            } else {
                &[0, 0, 0, 0]
            });
        }
    }
    tauri::image::Image::new_owned(rgba, SIZE, SIZE)
}
