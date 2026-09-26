//! Direct3D 11 + DirectComposition + Direct2D device resources, and a small painter API.
//!
//! The swap chain is a premultiplied-alpha composition swap chain attached to the window through
//! DirectComposition, so areas painted transparent show the Mica backdrop.

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, Result};
use windows_numerics::{Matrix3x2, Vector2};

pub type Color = D2D1_COLOR_F;

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

pub fn hex(rgb: u32) -> Color {
    rgba((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 1.0)
}

pub fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }
}

struct Device {
    swap: IDXGISwapChain1,
    ctx: ID2D1DeviceContext,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    size: (u32, u32),
    has_target: bool,
}

pub struct Gfx {
    pub factory: ID2D1Factory1,
    pub dwrite: IDWriteFactory,
    hwnd: HWND,
    dev: Option<Device>,
    dpi: f32,
    /// Incremented whenever the device is recreated (cached device-dependent resources must be
    /// dropped).
    pub generation: u64,
    /// Hardware device (true) or the WARP software rasterizer.
    pub use_gpu: bool,
}

impl Gfx {
    pub fn new(hwnd: HWND) -> Result<Gfx> {
        let opts = D2D1_FACTORY_OPTIONS { debugLevel: D2D1_DEBUG_LEVEL_NONE };
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&opts))? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Gfx { factory, dwrite, hwnd, dev: None, dpi: 96.0, generation: 0, use_gpu: false })
    }

    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi;
        if let Some(d) = &self.dev {
            unsafe { d.ctx.SetDpi(dpi, dpi) };
        }
    }

    pub fn scale(&self) -> f32 {
        self.dpi / 96.0
    }

    fn create_device(&self, w: u32, h: u32) -> Result<Device> {
        let mut d3d: Option<ID3D11Device> = None;
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let driver = if self.use_gpu { D3D_DRIVER_TYPE_HARDWARE } else { D3D_DRIVER_TYPE_WARP };
        let hw = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )
        };
        if hw.is_err() {
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    flags,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut d3d),
                    None,
                    None,
                )?
            };
        }
        let d3d = d3d.expect("D3D11 device");
        let dxgi_device: IDXGIDevice = d3d.cast()?;
        let dxgi_factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))? };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w.max(1),
            Height: h.max(1),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        let swap = unsafe { dxgi_factory.CreateSwapChainForComposition(&d3d, &desc, None)? };
        let d2d_device = unsafe { self.factory.CreateDevice(&dxgi_device)? };
        let ctx = unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };
        unsafe {
            ctx.SetDpi(self.dpi, self.dpi);
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi_device)? };
        let target = unsafe { dcomp.CreateTargetForHwnd(self.hwnd, true)? };
        let visual = unsafe { dcomp.CreateVisual()? };
        unsafe {
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;
        }
        Ok(Device {
            swap,
            ctx,
            _dcomp: dcomp,
            _target: target,
            _visual: visual,
            size: (w.max(1), h.max(1)),
            has_target: false,
        })
    }

    fn bind_target(&self, d: &mut Device) -> Result<()> {
        let surface: IDXGISurface = unsafe { d.swap.GetBuffer(0)? };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: self.dpi,
            dpiY: self.dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let bmp = unsafe { d.ctx.CreateBitmapFromDxgiSurface(&surface, Some(&props))? };
        unsafe { d.ctx.SetTarget(&bmp) };
        d.has_target = true;
        Ok(())
    }

    /// Resizes the swap chain (physical pixels).
    pub fn resize(&mut self, w: u32, h: u32) {
        let (w, h) = (w.max(1), h.max(1));
        let Some(d) = self.dev.as_mut() else { return };
        if d.size == (w, h) {
            return;
        }
        unsafe { d.ctx.SetTarget(None) };
        d.has_target = false;
        let r = unsafe { d.swap.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)) };
        if r.is_err() {
            self.dev = None;
            return;
        }
        d.size = (w, h);
    }

    /// Runs `draw` against the device context and presents. Recreates the device if it was lost.
    pub fn frame(&mut self, w: u32, h: u32, draw: impl FnOnce(&ID2D1DeviceContext)) -> Result<()> {
        if self.dev.is_none() {
            self.dev = Some(self.create_device(w, h)?);
            self.generation += 1;
        }
        self.resize(w, h);
        let mut dev = match self.dev.take() {
            Some(d) => d,
            None => {
                self.generation += 1;
                self.create_device(w, h)?
            }
        };
        if !dev.has_target {
            self.bind_target(&mut dev)?;
        }
        let ctx = dev.ctx.clone();
        unsafe {
            ctx.BeginDraw();
            ctx.SetTransform(&Matrix3x2::identity());
        }
        draw(&ctx);
        let end = unsafe { ctx.EndDraw(None, None) };
        let present = unsafe { dev.swap.Present(1, DXGI_PRESENT(0)) };
        if end.is_err() || present.is_err() {
            // Device lost (driver update, GPU reset): rebuild on the next frame.
            self.dev = None;
            return Ok(());
        }
        self.dev = Some(dev);
        Ok(())
    }
}

/// Immediate-mode drawing helpers with one reusable brush.
pub struct Painter<'a> {
    pub ctx: &'a ID2D1DeviceContext,
    brush: ID2D1SolidColorBrush,
}

impl<'a> Painter<'a> {
    pub fn new(ctx: &'a ID2D1DeviceContext) -> Painter<'a> {
        let brush = unsafe { ctx.CreateSolidColorBrush(&rgba(0, 0, 0, 1.0), None).expect("brush") };
        Painter { ctx, brush }
    }

    pub fn brush(&self, c: Color) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&c) };
        &self.brush
    }

    pub fn clear(&self, c: Color) {
        unsafe { self.ctx.Clear(Some(&c)) };
    }

    pub fn fill(&self, r: Rect, c: Color) {
        unsafe { self.ctx.FillRectangle(&r.d2d(), self.brush(c)) };
    }

    pub fn fill_round(&self, r: Rect, radius: f32, c: Color) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.ctx.FillRoundedRectangle(&rr, self.brush(c)) };
    }

    pub fn stroke_round(&self, r: Rect, radius: f32, c: Color, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.ctx.DrawRoundedRectangle(&rr, self.brush(c), width, None) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, width: f32) {
        unsafe { self.ctx.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, self.brush(c), width, None) };
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        unsafe { self.ctx.FillEllipse(&e, self.brush(c)) };
    }

    pub fn text(&self, layout: &IDWriteTextLayout, x: f32, y: f32, c: Color) {
        unsafe {
            self.ctx.DrawTextLayout(
                Vector2 { X: x, Y: y },
                layout,
                self.brush(c),
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            )
        };
    }

    pub fn clip(&self, r: Rect) {
        unsafe { self.ctx.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn unclip(&self) {
        unsafe { self.ctx.PopAxisAlignedClip() };
    }

    pub fn bitmap(&self, bmp: &ID2D1Bitmap, r: Rect, opacity: f32) {
        unsafe {
            self.ctx.DrawBitmap(bmp, Some(&r.d2d()), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None)
        };
    }
}

impl Gfx {
    /// Switches between GPU and software rendering (the device is rebuilt on the next frame).
    pub fn set_gpu(&mut self, on: bool) {
        if self.use_gpu != on {
            self.use_gpu = on;
            self.dev = None;
        }
    }
}
