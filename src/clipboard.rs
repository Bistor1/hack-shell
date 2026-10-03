//! Wayland clipboard and primary selection.
//!
//! Selecting text offers it on the primary selection (middle-click in other
//! windows) and, when copy-on-select is on, also on the regular clipboard.
//! The offer is served by a background thread, so it stays available after the
//! highlight is cleared and until another app takes the selection.

use std::io::Read;

use log::warn;
use wl_clipboard_rs::copy::{self, MimeType as CopyMime, Options, Source};
use wl_clipboard_rs::paste::{self, MimeType as PasteMime, Seat};

pub fn publish(text: &str, also_clipboard: bool) {
    if text.is_empty() {
        return;
    }
    let bytes: Box<[u8]> = text.as_bytes().into();
    let kind = if also_clipboard {
        copy::ClipboardType::Both
    } else {
        copy::ClipboardType::Primary
    };
    let mut opts = Options::new();
    opts.clipboard(kind);
    if opts
        .copy(Source::Bytes(bytes.clone()), CopyMime::Text)
        .is_err()
    {
        // Compositor without primary-on-both: publish the two buffers separately.
        let mut primary = Options::new();
        primary.clipboard(copy::ClipboardType::Primary);
        if let Err(err) = primary.copy(Source::Bytes(bytes.clone()), CopyMime::Text) {
            warn!("primary selection: {err}");
        }
        if also_clipboard {
            let mut regular = Options::new();
            regular.clipboard(copy::ClipboardType::Regular);
            if let Err(err) = regular.copy(Source::Bytes(bytes), CopyMime::Text) {
                warn!("clipboard: {err}");
            }
        }
    }
}

pub fn copy_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut opts = Options::new();
    opts.clipboard(copy::ClipboardType::Regular);
    if let Err(err) = opts.copy(Source::Bytes(text.as_bytes().into()), CopyMime::Text) {
        warn!("clipboard: {err}");
    }
}

pub fn paste_primary() -> Option<String> {
    read(paste::ClipboardType::Primary)
}

pub fn paste_clipboard() -> Option<String> {
    read(paste::ClipboardType::Regular)
}

fn read(kind: paste::ClipboardType) -> Option<String> {
    match paste::get_contents(kind, Seat::Unspecified, PasteMime::Text) {
        Ok((mut pipe, _)) => {
            let mut buf = String::new();
            pipe.read_to_string(&mut buf).ok()?;
            if buf.is_empty() { None } else { Some(buf) }
        },
        Err(err) => {
            log::debug!("paste {kind:?}: {err}");
            None
        },
    }
}
