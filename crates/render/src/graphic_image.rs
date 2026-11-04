//! Pictures for graphic image layers: decoded once into premultiplied linear-light mip chains and
//! kept while the source stays the same.
//!
//! A layer's `source` is looked up among pictures registered from memory first
//! ([`register_image_data`]: pictures embedded in a graphics template, or any picture on the web
//! build), then read from disk on native builds. A file is decoded again when its size or
//! modification time changes. A source that is missing or does not decode has no picture; the
//! layer then draws nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::image::Image;

/// A decoded picture: level 0 at full size, each later level half the one before.
#[derive(Debug)]
pub struct Picture {
    /// Changes whenever the picture behind a source changes (raster cache key).
    pub id: u64,
    pub mips: Vec<Image>,
}

impl Picture {
    pub fn size(&self) -> (usize, usize) {
        self.mips.first().map_or((0, 0), |m| (m.w, m.h))
    }

    /// The level to sample when one device pixel covers `footprint` level-0 pixels.
    pub fn level(&self, footprint: f32) -> &Image {
        let l = if footprint > 1.0 { footprint.log2().floor() as usize } else { 0 };
        &self.mips[l.min(self.mips.len() - 1)]
    }
}

/// File stamp: length and modification time.
type Stamp = (u64, Option<std::time::SystemTime>);

#[derive(Default)]
struct Store {
    data: HashMap<String, Arc<Picture>>,
    /// Path → (stamp, last use, picture).
    files: HashMap<String, (Stamp, u64, Option<Arc<Picture>>)>,
}

const MAX_FILES: usize = 32;

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn next_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

fn decode(name: &str, bytes: &[u8]) -> Option<Picture> {
    let r = filmcraft_media::still::decode_raster(name, bytes).ok()?;
    let (w, h) = (r.width as usize, r.height as usize);
    if w == 0 || h == 0 || r.rgba.len() < w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let mut lut = [0.0f32; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = filmcraft_color::srgb_to_linear(i as f32 / 255.0);
    }
    let mut img = Image::new(w, h);
    for (d, s) in img.px.as_chunks_mut::<4>().0.iter_mut().zip(r.rgba.as_chunks::<4>().0) {
        let a = s[3] as f32 / 255.0;
        d[0] = lut[s[0] as usize] * a;
        d[1] = lut[s[1] as usize] * a;
        d[2] = lut[s[2] as usize] * a;
        d[3] = a;
    }
    let mut mips = vec![img];
    while let Some(last) = mips.last()
        && (last.w > 1 || last.h > 1)
        && mips.len() < 16
    {
        let next = last.downsample2();
        mips.push(next);
    }
    Some(Picture { id: next_id(), mips })
}

/// Decode `bytes` and keep the picture under `key` (an image layer source such as
/// `embedded:<id>`), replacing any picture registered there before. Returns whether it decoded.
pub fn register_image_data(key: &str, bytes: &[u8]) -> bool {
    let Some(p) = decode(key, bytes) else { return false };
    store().lock().unwrap_or_else(|e| e.into_inner()).data.insert(key.to_string(), Arc::new(p));
    true
}

/// Whether a picture is registered from memory under `key`.
pub fn is_registered(key: &str) -> bool {
    store().lock().unwrap_or_else(|e| e.into_inner()).data.contains_key(key)
}

/// The picture behind an image layer source, if there is one.
pub fn picture(source: &str) -> Option<Arc<Picture>> {
    if source.is_empty() {
        return None;
    }
    if let Some(p) = store().lock().unwrap_or_else(|e| e.into_inner()).data.get(source) {
        return Some(p.clone());
    }
    file_picture(source)
}

#[cfg(not(target_arch = "wasm32"))]
fn file_picture(path: &str) -> Option<Arc<Picture>> {
    let meta = std::fs::metadata(path).ok()?;
    let stamp: Stamp = (meta.len(), meta.modified().ok());
    if let Some(e) = store().lock().unwrap_or_else(|e| e.into_inner()).files.get_mut(path)
        && e.0 == stamp
    {
        e.1 = next_id();
        return e.2.clone();
    }
    // decode outside the lock; a file that fails stays failed until it changes
    let p = std::fs::read(path).ok().and_then(|b| decode(path, &b)).map(Arc::new);
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    if s.files.len() >= MAX_FILES
        && !s.files.contains_key(path)
        && let Some(k) = s.files.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| k.clone())
    {
        s.files.remove(&k);
    }
    s.files.insert(path.to_string(), (stamp, next_id(), p.clone()));
    p
}

#[cfg(target_arch = "wasm32")]
fn file_picture(_path: &str) -> Option<Arc<Picture>> {
    None
}
