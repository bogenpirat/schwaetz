//! Image decoding through the Windows Imaging Component (PNG, JPEG, GIF, BMP, WebP, …).

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::GUID;

/// Initializes COM for the calling thread (idempotent).
pub fn init_thread() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// Decodes the first frame, scaled to fit within `max_dim`, as premultiplied BGRA.
pub fn decode(bytes: &[u8], max_dim: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let e = |e: windows::core::Error| e.message().to_string();
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(e)?;
        let stream = factory.CreateStream().map_err(e)?;
        stream.InitializeFromMemory(bytes).map_err(e)?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null::<GUID>(), WICDecodeMetadataCacheOnDemand)
            .map_err(e)?;
        let frame = decoder.GetFrame(0).map_err(e)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(e)?;
        if w == 0 || h == 0 || w > 16_384 || h > 16_384 {
            return Err(format!("unsupported image size {w}x{h}"));
        }
        let max_dim = max_dim.max(1);
        let source: IWICBitmapSource = if w.max(h) > max_dim {
            let scale = max_dim as f64 / w.max(h) as f64;
            let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
            let scaler = factory.CreateBitmapScaler().map_err(e)?;
            scaler.Initialize(&frame, nw, nh, WICBitmapInterpolationModeFant).map_err(e)?;
            w = nw;
            h = nh;
            scaler.into()
        } else {
            frame.into()
        };
        let conv = factory.CreateFormatConverter().map_err(e)?;
        conv.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )
        .map_err(e)?;
        let stride = w * 4;
        let mut buf = vec![0u8; (stride * h) as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut buf).map_err(e)?;
        Ok((w, h, buf))
    }
}
