//! Decoded images as Direct2D bitmaps, inline emote objects, link-preview state and the
//! placeholder of an image being uploaded.

use schwaetz_media::PageMeta;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_core::{BOOL, IUnknown, Ref, implement};

enum Img {
    /// Decoded frames waiting for a device context.
    Pixels {
        w: u32,
        h: u32,
        frames: Vec<schwaetz_media::Frame>,
    },
    /// Bitmaps with their display times; one entry for still images.
    Ready {
        frames: Vec<(ID2D1Bitmap1, u32)>,
        w: u32,
        h: u32,
    },
    Failed,
}

/// What we know about a link preview.
#[derive(Clone)]
pub enum Preview {
    /// A web page with metadata; `image` (if any) is loaded through the image store.
    Page(PageMeta),
    /// The URL itself is an image.
    Image,
    Failed,
}

#[derive(Default)]
pub struct ImageStore {
    images: HashMap<String, Img>,
    order: Vec<String>,
    bytes: usize,
    generation: u64,
    /// URLs the renderer needs: (url, is_preview).
    pub wants: Vec<(String, bool)>,
    pub previews: HashMap<String, Preview>,
    /// Link previews the user asked for (click-to-load).
    pub requested: std::collections::HashSet<String>,
    /// Animation clock (ms), advanced by the shell while the window is focused.
    pub clock: u64,
    /// Set when an animated image was drawn (the shell then keeps repainting).
    pub animating: std::cell::Cell<bool>,
}

const BUDGET: usize = 48 << 20;

impl ImageStore {
    pub fn insert_frames(&mut self, url: String, w: u32, h: u32, frames: Vec<schwaetz_media::Frame>) {
        if frames.is_empty() {
            return self.insert_failed(url);
        }
        self.bytes += frames.iter().map(|f| f.bgra.len()).sum::<usize>();
        self.touch(&url);
        self.images.insert(url, Img::Pixels { w, h, frames });
        self.evict();
    }

    pub fn insert_failed(&mut self, url: String) {
        self.images.insert(url, Img::Failed);
    }

    fn touch(&mut self, url: &str) {
        if let Some(i) = self.order.iter().position(|u| u == url) {
            let u = self.order.remove(i);
            self.order.push(u);
        } else {
            self.order.push(url.to_owned());
        }
    }

    fn evict(&mut self) {
        while self.bytes > BUDGET && self.order.len() > 1 {
            let url = self.order.remove(0);
            if let Some(img) = self.images.remove(&url) {
                self.bytes -= match img {
                    Img::Pixels { frames, .. } => frames.iter().map(|f| f.bgra.len()).sum(),
                    Img::Ready { w, h, frames } => (w * h * 4) as usize * frames.len(),
                    Img::Failed => 0,
                };
            }
        }
    }

    /// Size of a loaded image, requesting it if unknown.
    pub fn size(&mut self, url: &str, preview: bool) -> Option<(u32, u32)> {
        match self.images.get(url) {
            Some(Img::Pixels { w, h, .. } | Img::Ready { w, h, .. }) => Some((*w, *h)),
            Some(Img::Failed) => None,
            None => {
                if !self.wants.iter().any(|(u, _)| u == url) {
                    self.wants.push((url.to_owned(), preview));
                }
                None
            }
        }
    }

    pub fn failed(&self, url: &str) -> bool {
        matches!(self.images.get(url), Some(Img::Failed))
    }

    /// Converts pending pixels to bitmaps on the current device (dropping all bitmaps when the
    /// device was recreated; they are re-requested from the disk cache).
    pub fn realize(&mut self, dc: &ID2D1DeviceContext, generation: u64) {
        if generation != self.generation {
            self.generation = generation;
            self.images.retain(|_, i| !matches!(i, Img::Ready { .. }));
            self.bytes = self
                .images
                .values()
                .map(|i| if let Img::Pixels { frames, .. } = i { frames.iter().map(|f| f.bgra.len()).sum() } else { 0 })
                .sum();
            self.order.retain(|u| self.images.contains_key(u));
        }
        for img in self.images.values_mut() {
            if let Img::Pixels { w, h, frames } = img {
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let size = D2D_SIZE_U { width: *w, height: *h };
                let bitmaps: Result<Vec<(ID2D1Bitmap1, u32)>, _> = frames
                    .iter()
                    .map(|f| unsafe {
                        dc.CreateBitmap(size, Some(f.bgra.as_ptr() as *const _), *w * 4, &props)
                            .map(|b| (b, f.delay_ms.max(20)))
                    })
                    .collect();
                *img = match bitmaps {
                    Ok(frames) => Img::Ready { frames, w: *w, h: *h },
                    Err(_) => Img::Failed,
                };
            }
        }
    }

    /// The bitmap to show now (animated images follow `clock`).
    pub fn bitmap(&self, url: &str) -> Option<ID2D1Bitmap1> {
        let Some(Img::Ready { frames, .. }) = self.images.get(url) else { return None };
        if frames.len() == 1 {
            return Some(frames[0].0.clone());
        }
        self.animating.set(true);
        let total: u64 = frames.iter().map(|f| f.1 as u64).sum();
        let mut t = self.clock % total.max(1);
        for (bmp, d) in frames {
            if t < *d as u64 {
                return Some(bmp.clone());
            }
            t -= *d as u64;
        }
        frames.last().map(|f| f.0.clone())
    }
}

/// An inline image inside a text layout (emotes). Its advance width follows the image's
/// aspect ratio once known.
#[implement(IDWriteInlineObject)]
pub struct InlineImage {
    url: String,
    /// Advance width.
    width: f32,
    /// Width the image is drawn at (from the left).
    draw_w: f32,
    height: f32,
    baseline: f32,
    store: Rc<RefCell<ImageStore>>,
    dc: ID2D1DeviceContext,
}

impl InlineImage {
    pub fn create(
        url: &str,
        height: f32,
        baseline: f32,
        store: &Rc<RefCell<ImageStore>>,
        dc: &ID2D1DeviceContext,
    ) -> IDWriteInlineObject {
        let (w, h) = store.borrow_mut().size(url, false).unwrap_or((1, 1));
        let width = (height * w as f32 / h.max(1) as f32).clamp(height * 0.5, height * 4.0);
        InlineImage {
            url: url.to_owned(),
            width,
            draw_w: width,
            height,
            baseline,
            store: store.clone(),
            dc: dc.clone(),
        }
        .into()
    }

    /// A square image of `size` followed by `gap` (chat badges): its width is known before the
    /// image has loaded.
    pub fn square(
        url: &str,
        size: f32,
        gap: f32,
        baseline: f32,
        store: &Rc<RefCell<ImageStore>>,
        dc: &ID2D1DeviceContext,
    ) -> IDWriteInlineObject {
        store.borrow_mut().size(url, false);
        let (url, store, dc) = (url.to_owned(), store.clone(), dc.clone());
        InlineImage { url, width: size + gap, draw_w: size, height: size, baseline, store, dc }.into()
    }
}

impl IDWriteInlineObject_Impl for InlineImage_Impl {
    fn Draw(
        &self,
        _ctx: *const core::ffi::c_void,
        _renderer: Ref<IDWriteTextRenderer>,
        x: f32,
        y: f32,
        _sideways: BOOL,
        _rtl: BOOL,
        _effect: Ref<IUnknown>,
    ) -> windows_core::Result<()> {
        let bmp = self.store.try_borrow().ok().and_then(|s| s.bitmap(&self.url));
        if let Some(bmp) = bmp {
            let r = D2D_RECT_F { left: x, top: y, right: x + self.draw_w, bottom: y + self.height };
            unsafe { self.dc.DrawBitmap(&bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
        }
        Ok(())
    }

    fn GetMetrics(&self) -> windows_core::Result<DWRITE_INLINE_OBJECT_METRICS> {
        Ok(DWRITE_INLINE_OBJECT_METRICS {
            width: self.width,
            height: self.height,
            baseline: self.baseline,
            supportsSideways: false.into(),
        })
    }

    fn GetOverhangMetrics(&self) -> windows_core::Result<DWRITE_OVERHANG_METRICS> {
        Ok(DWRITE_OVERHANG_METRICS::default())
    }

    // The signature is fixed by the COM interface; pointers are checked for null.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn GetBreakConditions(
        &self,
        before: *mut DWRITE_BREAK_CONDITION,
        after: *mut DWRITE_BREAK_CONDITION,
    ) -> windows_core::Result<()> {
        if before.is_null() || after.is_null() {
            return Err(E_FAIL.into());
        }
        unsafe {
            *before = DWRITE_BREAK_CONDITION_NEUTRAL;
            *after = DWRITE_BREAK_CONDITION_NEUTRAL;
        }
        Ok(())
    }
}

/// State of one upload, shared with its worker thread.
#[derive(Default)]
pub struct UploadProgress {
    /// Bytes of the request sent so far.
    pub sent: std::sync::atomic::AtomicU64,
    /// Size of the request (0 until it has been prepared).
    pub total: std::sync::atomic::AtomicU64,
    /// Set by the UI to abort.
    pub cancel: std::sync::atomic::AtomicBool,
}

impl UploadProgress {
    fn label(&self) -> (String, f32) {
        use std::sync::atomic::Ordering::Relaxed;
        let (sent, total) = (self.sent.load(Relaxed), self.total.load(Relaxed));
        match (sent, total) {
            (_, 0) => (UPLOAD_PREPARING.to_owned(), 0.0),
            (s, t) if s >= t => ("Finishing…".to_owned(), 1.0),
            (s, t) => (format!("Uploading… {} %", s * 100 / t), s as f32 / t as f32),
        }
    }
}

/// The widest label; the placeholder is sized for it.
const UPLOAD_PREPARING: &str = "Preparing image…";

/// The placeholder shown in the input box where the link of an image being uploaded will go:
/// a pill with the state of the upload, filling up from the left.
#[implement(IDWriteInlineObject)]
pub struct UploadChip {
    progress: std::sync::Arc<UploadProgress>,
    dwrite: IDWriteFactory,
    format: IDWriteTextFormat,
    width: f32,
    height: f32,
    baseline: f32,
    colors: (D2D1_COLOR_F, D2D1_COLOR_F, D2D1_COLOR_F),
    dc: ID2D1DeviceContext,
}

impl UploadChip {
    /// `colors` are the pill, its filled part and the text.
    pub fn create(
        progress: std::sync::Arc<UploadProgress>,
        text: &crate::text::Text,
        colors: (D2D1_COLOR_F, D2D1_COLOR_F, D2D1_COLOR_F),
        dc: &ID2D1DeviceContext,
    ) -> IDWriteInlineObject {
        let f = &text.fonts;
        // As tall as a line of the text around it, so the input box keeps its height.
        let mut line = [DWRITE_LINE_METRICS::default()];
        let mut lines = 0u32;
        unsafe {
            let _ = text.layout("X", &f.chat, 100.0, 100.0).GetLineMetrics(Some(&mut line), &mut lines);
        }
        let (height, baseline) =
            if lines > 0 { (line[0].height, line[0].baseline) } else { (f.line_height, f.line_height * 0.8) };
        let label = text.layout(UPLOAD_PREPARING, &f.ui_small, 400.0, 40.0);
        UploadChip {
            progress,
            dwrite: text.dwrite.clone(),
            format: f.ui_small.clone(),
            width: crate::text::metrics(&label).width + 24.0,
            height,
            baseline,
            colors,
            dc: dc.clone(),
        }
        .into()
    }
}

impl IDWriteInlineObject_Impl for UploadChip_Impl {
    fn Draw(
        &self,
        _ctx: *const core::ffi::c_void,
        _renderer: Ref<IDWriteTextRenderer>,
        x: f32,
        y: f32,
        _sideways: BOOL,
        _rtl: BOOL,
        _effect: Ref<IUnknown>,
    ) -> windows_core::Result<()> {
        use crate::gfx::{Painter, Rect};
        let (label, done) = self.progress.label();
        let (bg, fill, fg) = self.colors;
        let pill = Rect::new(x + 2.0, y + 1.0, self.width - 4.0, self.height - 2.0);
        let p = Painter::new(&self.dc);
        p.fill_round(pill, 6.0, bg);
        if done > 0.0 {
            p.clip(Rect::new(pill.x, pill.y, pill.w * done.min(1.0), pill.h));
            p.fill_round(pill, 6.0, fill);
            p.unclip();
        }
        let wide: Vec<u16> = label.encode_utf16().collect();
        if let Ok(l) = unsafe { self.dwrite.CreateTextLayout(&wide, &self.format, pill.w, pill.h) } {
            let m = crate::text::metrics(&l);
            p.text(&l, pill.x + (pill.w - m.width) / 2.0, pill.y + (pill.h - m.height) / 2.0, fg);
        }
        Ok(())
    }

    fn GetMetrics(&self) -> windows_core::Result<DWRITE_INLINE_OBJECT_METRICS> {
        Ok(DWRITE_INLINE_OBJECT_METRICS {
            width: self.width,
            height: self.height,
            baseline: self.baseline,
            supportsSideways: false.into(),
        })
    }

    fn GetOverhangMetrics(&self) -> windows_core::Result<DWRITE_OVERHANG_METRICS> {
        Ok(DWRITE_OVERHANG_METRICS::default())
    }

    // The signature is fixed by the COM interface; pointers are checked for null.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn GetBreakConditions(
        &self,
        before: *mut DWRITE_BREAK_CONDITION,
        after: *mut DWRITE_BREAK_CONDITION,
    ) -> windows_core::Result<()> {
        if before.is_null() || after.is_null() {
            return Err(E_FAIL.into());
        }
        unsafe {
            *before = DWRITE_BREAK_CONDITION_NEUTRAL;
            *after = DWRITE_BREAK_CONDITION_NEUTRAL;
        }
        Ok(())
    }
}
