//! WIC decoding against PNGs built in-process (uncompressed deflate, so no zlib needed).

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut c = kind.to_vec();
    c.extend_from_slice(data);
    out.extend_from_slice(&c);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

/// RGBA pixels → PNG.
fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[test]
fn decodes_and_premultiplies() {
    // 2x1: opaque red, half-transparent white.
    let bytes = png(2, 1, &[255, 0, 0, 255, 255, 255, 255, 128]);
    let (w, h, bgra) = schwaetz_media::decode(&bytes, 64).unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(&bgra[..4], &[0, 0, 255, 255]);
    assert_eq!(bgra[7], 128);
    assert!((bgra[4] as i32 - 128).abs() <= 1, "premultiplied: {:?}", &bgra[4..8]);
}

#[test]
fn scales_to_fit() {
    let (w, h) = (400u32, 100u32);
    let bytes = png(w, h, &vec![200u8; (w * h * 4) as usize]);
    let (sw, sh, bgra) = schwaetz_media::decode(&bytes, 100).unwrap();
    assert_eq!((sw, sh), (100, 25));
    assert_eq!(bgra.len(), 100 * 25 * 4);
}

#[test]
fn rejects_garbage() {
    assert!(schwaetz_media::decode(b"definitely not an image", 64).is_err());
}

/// A GIF89a with 128-color palette and "uncompressed" LZW (8-bit codes, clear every 126 pixels).
/// frames: (left, top, w, h, disposal, delay_cs, palette indices)
fn gif(w: u16, h: u16, palette: &[[u8; 3]], frames: &[(u16, u16, u16, u16, u8, u16, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&[0xF6, 0, 0]); // global table, 128 entries
    for i in 0..128 {
        out.extend_from_slice(palette.get(i).unwrap_or(&[0, 0, 0]));
    }
    for (l, t, fw, fh, disposal, delay, px) in frames {
        out.extend_from_slice(&[0x21, 0xF9, 4, disposal << 2]);
        out.extend_from_slice(&delay.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.push(0x2C);
        for v in [l, t, fw, fh] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.push(0);
        out.push(7); // LZW minimum code size
        let mut codes = Vec::new();
        for (i, p) in px.iter().enumerate() {
            if i % 126 == 0 {
                codes.push(128);
            }
            codes.push(*p);
        }
        codes.push(129);
        for chunk in codes.chunks(255) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        out.push(0);
    }
    out.push(0x3B);
    out
}

#[test]
fn animated_gif_frames_are_composited() {
    let palette = [[255, 0, 0], [0, 0, 255]];
    let bytes = gif(4, 4, &palette, &[(0, 0, 4, 4, 1, 5, vec![0; 16]), (2, 2, 2, 2, 1, 7, vec![1; 4])]);
    let (w, h, frames) = schwaetz_media::decode_frames(&bytes, 64).unwrap();
    assert_eq!((w, h), (4, 4));
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].delay_ms, 50);
    assert_eq!(frames[1].delay_ms, 70);
    let px = |f: &schwaetz_media::Frame, x: usize, y: usize| f.bgra[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4].to_vec();
    assert_eq!(px(&frames[1], 0, 0), vec![0, 0, 255, 255], "untouched area keeps frame 1 (red)");
    assert_eq!(px(&frames[1], 3, 3), vec![255, 0, 0, 255], "patched area is blue");
}
