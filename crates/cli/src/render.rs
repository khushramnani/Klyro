//! Pictures in a terminal, as pure functions: decode a PNG, scale it, and draw it as coloured
//! half-block characters (any terminal) or sixel (Windows Terminal, foot, WezTerm, ...).

/// RGB pixels, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

impl Image {
    pub fn decode(png_bytes: &[u8]) -> Result<Image, String> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(png_bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(|e| format!("bad frame: {e}"))?;
        let mut buf = vec![0; reader.output_buffer_size().ok_or("frame too large")?];
        let info = reader.next_frame(&mut buf).map_err(|e| format!("bad frame: {e}"))?;
        let (w, h) = (info.width as usize, info.height as usize);
        let px = |i: usize| -> [u8; 3] {
            match info.color_type {
                png::ColorType::Rgba => [buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2]],
                png::ColorType::Rgb => [buf[i * 3], buf[i * 3 + 1], buf[i * 3 + 2]],
                png::ColorType::GrayscaleAlpha => [buf[i * 2]; 3],
                _ => [buf[i]; 3],
            }
        };
        Ok(Image { w, h, rgb: (0..w * h).flat_map(px).collect() })
    }

    fn at(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    /// Box-averaged down (or nearest up) to w x h, so thin text stays visible as grey.
    pub fn scale(&self, w: usize, h: usize) -> Image {
        let (w, h) = (w.max(1), h.max(1));
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            let (y0, y1) = (y * self.h / h, ((y + 1) * self.h / h).max(y * self.h / h + 1));
            for x in 0..w {
                let (x0, x1) = (x * self.w / w, ((x + 1) * self.w / w).max(x * self.w / w + 1));
                let mut sum = [0u32; 3];
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let p = self.at(xx, yy);
                        (0..3).for_each(|c| sum[c] += p[c] as u32);
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u32;
                rgb.extend(sum.map(|s| (s / n) as u8));
            }
        }
        Image { w, h, rgb }
    }
}

/// The largest w x h with the image's aspect ratio that fits in max_w x max_h.
pub fn fit(img_w: usize, img_h: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    if img_w * max_h <= max_w * img_h {
        ((img_w * max_h / img_h.max(1)).max(1), max_h)
    } else {
        (max_w, (img_h * max_w / img_w.max(1)).max(1))
    }
}

/// One string per terminal row: each cell is `▀` with the top pixel as foreground and the
/// bottom one as background, so a cell shows two square-ish pixels. `img` is cols x 2*rows.
pub fn blocks(img: &Image) -> Vec<String> {
    let mut rows = vec![];
    for y in (0..img.h).step_by(2) {
        let mut s = String::new();
        let mut last = None;
        for x in 0..img.w {
            let (top, bottom) = (img.at(x, y), if y + 1 < img.h { img.at(x, y + 1) } else { [0; 3] });
            if last != Some((top, bottom)) {
                s += &format!("\x1b[38;2;{};{};{};48;2;{};{};{}m", top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]);
                last = Some((top, bottom));
            }
            s.push('▀');
        }
        s += "\x1b[0m";
        rows.push(s);
    }
    rows
}

/// A sixel image, colours rounded to a fixed 6x6x6 cube (216 colours: plenty for web pages,
/// and no per-frame palette search). ponytail: fixed palette; median-cut if photos look poor.
pub fn sixel(img: &Image) -> String {
    let cube = |v: u8| (v as usize * 5 + 127) / 255;
    let idx: Vec<usize> = (0..img.w * img.h).map(|i| cube(img.rgb[i * 3]) * 36 + cube(img.rgb[i * 3 + 1]) * 6 + cube(img.rgb[i * 3 + 2])).collect();
    let mut used = [false; 216];
    idx.iter().for_each(|&c| used[c] = true);
    let mut out = format!("\x1bPq\"1;1;{};{}", img.w, img.h);
    for (c, _) in used.iter().enumerate().filter(|(_, u)| **u) {
        let pct = |n: usize| n * 100 / 5;
        out += &format!("#{c};2;{};{};{}", pct(c / 36), pct(c / 6 % 6), pct(c % 6));
    }
    for band in (0..img.h).step_by(6) {
        let rows = (img.h - band).min(6);
        let mut present = [false; 216];
        for y in band..band + rows {
            idx[y * img.w..(y + 1) * img.w].iter().for_each(|&c| present[c] = true);
        }
        let mut first = true;
        for c in (0..216).filter(|&c| present[c]) {
            if !first {
                out.push('$'); // back to the band's start, next colour on top
            }
            first = false;
            out += &format!("#{c}");
            let mut run = (0u8, 0usize);
            for x in 0..img.w {
                let bits = (0..rows).filter(|&r| idx[(band + r) * img.w + x] == c).fold(0u8, |b, r| b | 1 << r);
                if bits == run.0 || run.1 == 0 {
                    run = (bits, run.1 + 1);
                } else {
                    push_run(&mut out, run);
                    run = (bits, 1);
                }
            }
            push_run(&mut out, run);
        }
        out.push('-'); // next band
    }
    out + "\x1b\\"
}

fn push_run(out: &mut String, (bits, n): (u8, usize)) {
    let ch = (63 + bits) as char;
    if n > 3 {
        *out += &format!("!{n}{ch}");
    } else {
        (0..n).for_each(|_| out.push(ch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: usize, h: usize, px: &[[u8; 3]]) -> Image {
        Image { w, h, rgb: px.iter().flatten().copied().collect() }
    }

    #[test]
    fn decode_png() {
        let mut bytes = vec![];
        let mut enc = png::Encoder::new(&mut bytes, 2, 1);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(&[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        assert_eq!(Image::decode(&bytes).unwrap(), img(2, 1, &[[255, 0, 0], [0, 0, 255]]));
        assert!(Image::decode(b"nope").is_err());
    }

    #[test]
    fn scale_averages_and_fit_keeps_aspect() {
        let i = img(2, 2, &[[0; 3], [255; 3], [255; 3], [255; 3]]);
        assert_eq!(i.scale(1, 1).rgb, [191, 191, 191]);
        assert_eq!(i.scale(4, 4).w, 4);
        assert_eq!(fit(1280, 800, 100, 100), (100, 62));
        assert_eq!(fit(1280, 800, 300, 50), (80, 50));
    }

    #[test]
    fn blocks_pair_rows_and_skip_repeated_colours() {
        let (r, b) = ([255, 0, 0], [0, 0, 255]);
        let rows = blocks(&img(2, 2, &[r, r, b, b]));
        assert_eq!(rows, ["\x1b[38;2;255;0;0;48;2;0;0;255m▀▀\x1b[0m"]);
        assert_eq!(blocks(&img(1, 3, &[r, b, r])).len(), 2, "odd height: last row has a black bottom");
    }

    #[test]
    fn sixel_bands_runs_and_palette() {
        // 5 wide, 6 tall, all white: one colour (215), one band, a run of 5 full columns.
        let s = sixel(&img(5, 6, &[[255; 3]; 30]));
        assert_eq!(s, "\x1bPq\"1;1;5;6#215;2;100;100;100#215!5~-\x1b\\");
        // Two colours in one band: the second is drawn after `$`.
        let s = sixel(&img(2, 1, &[[0; 3], [255; 3]]));
        assert_eq!(s, "\x1bPq\"1;1;2;1#0;2;0;0;0#215;2;100;100;100#0@?$#215?@-\x1b\\");
    }
}
