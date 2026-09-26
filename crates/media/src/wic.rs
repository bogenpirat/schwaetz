//! Image decoding through the Windows Imaging Component (PNG, JPEG, GIF, BMP, WebP, …),
//! including animated GIFs (frames composited according to their offsets and disposal).

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::{GUID, HSTRING, Interface};

/// Frames beyond this are dropped (long animations are rare for emotes and previews).
const MAX_FRAMES: usize = 120;
/// Upper bound for all decoded frames of one image.
const MAX_BYTES: usize = 16 << 20;

#[derive(Debug)]
pub struct Frame {
    pub bgra: Vec<u8>,
    /// Display time in milliseconds (0 for still images).
    pub delay_ms: u32,
}

/// Initializes COM for the calling thread (idempotent).
pub fn init_thread() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

unsafe fn meta_u16(reader: &IWICMetadataQueryReader, name: &str) -> Option<u16> {
    let mut pv = PROPVARIANT::default();
    let name = HSTRING::from(name);
    unsafe {
        reader.GetMetadataByName(&name, &mut pv).ok()?;
        let inner = &pv.Anonymous.Anonymous;
        let v = match inner.vt.0 {
            18 => Some(inner.Anonymous.uiVal),        // VT_UI2
            17 => Some(inner.Anonymous.bVal as u16),  // VT_UI1
            19 => Some(inner.Anonymous.ulVal as u16), // VT_UI4
            _ => None,
        };
        let _ = PropVariantClear(&mut pv);
        v
    }
}

/// Converts a WIC source to premultiplied BGRA scaled to `w`×`h`.
unsafe fn to_bgra(factory: &IWICImagingFactory, src: &IWICBitmapSource, w: u32, h: u32) -> Result<Vec<u8>, String> {
    unsafe {
        let (mut sw, mut sh) = (0, 0);
        src.GetSize(&mut sw, &mut sh).map_err(err)?;
        let source: IWICBitmapSource = if (sw, sh) != (w, h) {
            let scaler = factory.CreateBitmapScaler().map_err(err)?;
            scaler.Initialize(src, w, h, WICBitmapInterpolationModeFant).map_err(err)?;
            scaler.into()
        } else {
            src.clone()
        };
        let conv = factory.CreateFormatConverter().map_err(err)?;
        conv.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )
        .map_err(err)?;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut buf).map_err(err)?;
        Ok(buf)
    }
}

fn fit(w: u32, h: u32, max_dim: u32) -> (u32, u32) {
    let max_dim = max_dim.max(1);
    if w.max(h) <= max_dim {
        return (w, h);
    }
    let s = max_dim as f64 / w.max(h) as f64;
    (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
}

/// Decodes all frames (one for still images), scaled to fit within `max_dim`.
pub fn decode_frames(bytes: &[u8], max_dim: u32) -> Result<(u32, u32, Vec<Frame>), String> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(err)?;
        let stream = factory.CreateStream().map_err(err)?;
        stream.InitializeFromMemory(bytes).map_err(err)?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null::<GUID>(), WICDecodeMetadataCacheOnDemand)
            .map_err(err)?;
        let count = decoder.GetFrameCount().map_err(err)? as usize;
        let is_gif = decoder.GetContainerFormat().map_err(err)? == GUID_ContainerFormatGif;
        let first = decoder.GetFrame(0).map_err(err)?;
        let (mut fw, mut fh) = (0u32, 0u32);
        first.GetSize(&mut fw, &mut fh).map_err(err)?;
        // A GIF's canvas is its logical screen, which frames are positioned on.
        let (cw, ch) = if is_gif && count > 1 {
            let r = decoder.GetMetadataQueryReader().ok();
            let lw = r.as_ref().and_then(|r| meta_u16(r, "/logscrdesc/Width")).map(u32::from).unwrap_or(fw);
            let lh = r.as_ref().and_then(|r| meta_u16(r, "/logscrdesc/Height")).map(u32::from).unwrap_or(fh);
            (lw.max(1), lh.max(1))
        } else {
            (fw, fh)
        };
        if cw == 0 || ch == 0 || cw > 16_384 || ch > 16_384 {
            return Err(format!("unsupported image size {cw}x{ch}"));
        }
        let (ow, oh) = fit(cw, ch, max_dim);

        if count <= 1 || !is_gif {
            // Still image (or an animated format without positioning metadata: frames are full).
            let mut frames = Vec::new();
            let mut total = 0usize;
            for i in 0..count.clamp(1, MAX_FRAMES) {
                let f = decoder.GetFrame(i as u32).map_err(err)?;
                let bgra = to_bgra(&factory, &f.cast().map_err(err)?, ow, oh)?;
                total += bgra.len();
                if total > MAX_BYTES && !frames.is_empty() {
                    break;
                }
                let delay = if count > 1 { 100 } else { 0 };
                frames.push(Frame { bgra, delay_ms: delay });
            }
            return Ok((ow, oh, frames));
        }

        // Animated GIF: composite onto a full-size canvas.
        let mut canvas = vec![0u8; (cw * ch * 4) as usize];
        let mut frames = Vec::new();
        let mut total = 0usize;
        let mut prev: Option<(u32, u32, u32, u32, u16, Vec<u8>)> = None;
        for i in 0..count.min(MAX_FRAMES) {
            // Apply the previous frame's disposal.
            if let Some((l, t, w, h, disposal, saved)) = prev.take() {
                match disposal {
                    2 => clear_rect(&mut canvas, cw, l, t, w, h),
                    3 => canvas = saved,
                    _ => {}
                }
            }
            let f = decoder.GetFrame(i as u32).map_err(err)?;
            let (mut w, mut h) = (0u32, 0u32);
            f.GetSize(&mut w, &mut h).map_err(err)?;
            let r = f.GetMetadataQueryReader().ok();
            let get = |n: &str| r.as_ref().and_then(|r| meta_u16(r, n));
            let (l, t) = (get("/imgdesc/Left").unwrap_or(0) as u32, get("/imgdesc/Top").unwrap_or(0) as u32);
            let delay = get("/grctlext/Delay").unwrap_or(10) as u32 * 10;
            let disposal = get("/grctlext/Disposal").unwrap_or(0);
            let saved = if disposal == 3 { canvas.clone() } else { Vec::new() };
            let pixels = to_bgra(&factory, &f.cast().map_err(err)?, w, h)?;
            blend(&mut canvas, cw, ch, &pixels, l, t, w, h);
            prev = Some((l, t, w, h, disposal, saved));

            let out = if (ow, oh) == (cw, ch) {
                canvas.clone()
            } else {
                let bmp = factory
                    .CreateBitmapFromMemory(cw, ch, &GUID_WICPixelFormat32bppPBGRA, cw * 4, &canvas)
                    .map_err(err)?;
                to_bgra(&factory, &bmp.cast().map_err(err)?, ow, oh)?
            };
            total += out.len();
            if total > MAX_BYTES {
                break;
            }
            // Browsers treat very short delays as 100 ms; do the same.
            frames.push(Frame { bgra: out, delay_ms: if delay < 20 { 100 } else { delay } });
        }
        Ok((ow, oh, frames))
    }
}

fn clear_rect(canvas: &mut [u8], cw: u32, l: u32, t: u32, w: u32, h: u32) {
    let ch = canvas.len() as u32 / 4 / cw.max(1);
    for y in t..(t + h).min(ch) {
        let start = ((y * cw + l.min(cw)) * 4) as usize;
        let end = ((y * cw + (l + w).min(cw)) * 4) as usize;
        canvas[start..end].fill(0);
    }
}

/// Premultiplied "source over" of a frame onto the canvas at (l, t).
#[allow(clippy::too_many_arguments)]
fn blend(canvas: &mut [u8], cw: u32, ch: u32, src: &[u8], l: u32, t: u32, w: u32, h: u32) {
    for y in 0..h {
        let cy = t + y;
        if cy >= ch {
            break;
        }
        for x in 0..w {
            let cx = l + x;
            if cx >= cw {
                break;
            }
            let s = ((y * w + x) * 4) as usize;
            let d = ((cy * cw + cx) * 4) as usize;
            let a = src[s + 3] as u32;
            if a == 255 {
                canvas[d..d + 4].copy_from_slice(&src[s..s + 4]);
            } else if a > 0 {
                for k in 0..4 {
                    canvas[d + k] = (src[s + k] as u32 + canvas[d + k] as u32 * (255 - a) / 255) as u8;
                }
            }
        }
    }
}

/// Decodes the first frame only.
pub fn decode(bytes: &[u8], max_dim: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let (w, h, mut frames) = decode_frames(bytes, max_dim)?;
    Ok((w, h, frames.swap_remove(0).bgra))
}
