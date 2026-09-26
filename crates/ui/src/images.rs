//! Decoded images as Direct2D bitmaps, inline emote objects and link-preview state.

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
    /// Decoded pixels waiting for a device context.
    Pixels {
        w: u32,
        h: u32,
        bgra: Vec<u8>,
    },
    Ready {
        bmp: ID2D1Bitmap1,
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
}

const BUDGET: usize = 48 << 20;

impl ImageStore {
    pub fn insert_pixels(&mut self, url: String, w: u32, h: u32, bgra: Vec<u8>) {
        self.bytes += bgra.len();
        self.touch(&url);
        self.images.insert(url, Img::Pixels { w, h, bgra });
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
                    Img::Pixels { bgra, .. } => bgra.len(),
                    Img::Ready { w, h, .. } => (w * h * 4) as usize,
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
            self.bytes =
                self.images.values().map(|i| if let Img::Pixels { bgra, .. } = i { bgra.len() } else { 0 }).sum();
            self.order.retain(|u| self.images.contains_key(u));
        }
        for img in self.images.values_mut() {
            if let Img::Pixels { w, h, bgra } = img {
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
                *img = match unsafe { dc.CreateBitmap(size, Some(bgra.as_ptr() as *const _), *w * 4, &props) } {
                    Ok(bmp) => Img::Ready { bmp, w: *w, h: *h },
                    Err(_) => Img::Failed,
                };
            }
        }
    }

    pub fn bitmap(&self, url: &str) -> Option<ID2D1Bitmap1> {
        match self.images.get(url) {
            Some(Img::Ready { bmp, .. }) => Some(bmp.clone()),
            _ => None,
        }
    }
}

/// An inline image inside a text layout (emotes). Its advance width follows the image's
/// aspect ratio once known.
#[implement(IDWriteInlineObject)]
pub struct InlineImage {
    url: String,
    width: f32,
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
        InlineImage { url: url.to_owned(), width, height, baseline, store: store.clone(), dc: dc.clone() }.into()
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
            let r = D2D_RECT_F { left: x, top: y, right: x + self.width, bottom: y + self.height };
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
