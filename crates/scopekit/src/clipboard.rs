//! The system clipboard: text and images (feature `clipboard`).
//!
//! This is the clipboard of the machine the app runs on. Over SSH that is
//! the remote machine's; text can then still be copied with the terminal's
//! own selection (Shift + drag while mouse reporting is on).

use std::cell::RefCell;

thread_local! {
    // Kept alive: on X11 and Wayland the content lives as long as the owner.
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

fn with<T>(
    f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, String> {
    CLIPBOARD.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = Some(arboard::Clipboard::new().map_err(|e| format!("no clipboard: {e}"))?);
        }
        match c.as_mut() {
            Some(cb) => f(cb).map_err(|e| format!("clipboard: {e}")),
            None => Err("no clipboard".into()),
        }
    })
}

/// Put `text` on the clipboard.
pub fn copy_text(text: &str) -> Result<(), String> {
    with(|c| c.set_text(text.to_string()))
}

/// The clipboard's text.
pub fn paste_text() -> Result<String, String> {
    with(arboard::Clipboard::get_text)
}

/// Put an image on the clipboard: tightly packed RGBA8 rows, `width` x
/// `height` (as [`render_to_rgba`](crate::render_to_rgba) returns them).
pub fn copy_image(rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return Err("pixels do not match the size".into());
    }
    with(|c| {
        c.set_image(arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: std::borrow::Cow::Borrowed(rgba),
        })
    })
}
