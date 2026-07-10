//! Shared high-fidelity drag-chip rasterizer used by every platform host.

use crate::ExternalDragPreview;

/// Logical chip size in pixels (matches the OS preview window).
pub const CHIP_WIDTH: usize = 224;
pub const CHIP_HEIGHT: usize = 90;

/// RGBA8 bitmap for a drag chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragChipImage {
    pub width: usize,
    pub height: usize,
    /// Premultiplied-friendly straight RGBA (A last).
    pub rgba: Vec<u8>,
}

impl DragChipImage {
    #[must_use]
    pub fn pixel_count(&self) -> usize {
        self.width.saturating_mul(self.height)
    }
}

/// Rasterize preview metadata into the shared chip look.
#[must_use]
pub fn render_drag_chip(preview: &ExternalDragPreview) -> DragChipImage {
    render_drag_chip_sized(preview, CHIP_WIDTH, CHIP_HEIGHT)
}

/// Rasterize at a custom size (static upright chip).
#[must_use]
pub fn render_drag_chip_sized(
    preview: &ExternalDragPreview,
    width: usize,
    height: usize,
) -> DragChipImage {
    let width = width.max(32);
    let height = height.max(24);
    let mut rgba = vec![0_u8; width * height * 4];
    clear(&mut rgba);
    draw_chip(&mut rgba, width, height, preview);
    DragChipImage {
        width,
        height,
        rgba,
    }
}

fn draw_chip(rgba: &mut [u8], width: usize, height: usize, preview: &ExternalDragPreview) {
    // Soft drop shadow
    fill_rounded(rgba, width, height, 6, 8, width.saturating_sub(8), height.saturating_sub(6), 10, [
        0, 0, 0, 55,
    ]);
    // Outer card
    fill_rounded(rgba, width, height, 4, 4, width.saturating_sub(8), height.saturating_sub(10), 12, [
        18, 16, 28, 245,
    ]);
    // Inner panel
    fill_rounded(
        rgba,
        width,
        height,
        10,
        10,
        width.saturating_sub(20),
        height.saturating_sub(22),
        8,
        [13, 19, 30, 255],
    );
    // Border highlight
    stroke_rounded(
        rgba,
        width,
        height,
        4,
        4,
        width.saturating_sub(8),
        height.saturating_sub(10),
        12,
        [204, 222, 238, 90],
    );

    match preview {
        ExternalDragPreview::Waveform { buckets } => draw_waveform(rgba, width, height, buckets),
        ExternalDragPreview::Spectral {
            columns,
            rows,
            energy,
            ..
        } => draw_spectral(rgba, width, height, *columns, *rows, energy),
        ExternalDragPreview::Midi { notes } => draw_midi(rgba, width, height, notes),
    }
}

fn draw_waveform(rgba: &mut [u8], width: usize, height: usize, buckets: &[(f32, f32)]) {
    let left = 16_usize;
    let right = width.saturating_sub(16);
    let top = 16_usize;
    let bottom = height.saturating_sub(18);
    let center = (top + bottom) / 2;
    hline(rgba, width, height, left, right, center, [75, 98, 124, 160]);

    if buckets.is_empty() {
        return;
    }
    let usable = right.saturating_sub(left).max(1);
    let amp = (bottom.saturating_sub(top) as f32) * 0.42;
    let len = buckets.len().saturating_sub(1).max(1);

    for (index, &(min, max)) in buckets.iter().enumerate() {
        let x = left + index * usable / len;
        let y1 = (center as f32 - max.clamp(-1.0, 1.0) * amp)
            .round()
            .clamp(top as f32, bottom as f32) as usize;
        let y2 = (center as f32 - min.clamp(-1.0, 1.0) * amp)
            .round()
            .clamp(top as f32, bottom as f32) as usize;
        let y = y1.min(y2);
        let h = y1.max(y2).saturating_sub(y).max(1);
        vbar(rgba, width, height, x, y, 3, h, [168, 107, 234, 70]);
        vbar(rgba, width, height, x, y, 2, h, [169, 222, 255, 235]);
    }
}

fn draw_spectral(
    rgba: &mut [u8],
    width: usize,
    height: usize,
    columns: usize,
    rows: usize,
    energy: &[f32],
) {
    if columns == 0 || rows == 0 || energy.is_empty() {
        return;
    }
    let left = 14_usize;
    let top = 14_usize;
    let usable_w = width.saturating_sub(28).max(1);
    let usable_h = height.saturating_sub(28).max(1);
    for y in 0..usable_h {
        // Low frequency at bottom (matches spectrogram convention).
        let row = rows
            .saturating_sub(1)
            .saturating_sub(y * rows / usable_h);
        for x in 0..usable_w {
            let column = x * columns / usable_w;
            // Column-major: energy[column * rows + row] (matches BUFFR writers).
            let value = energy
                .get(column.saturating_mul(rows).saturating_add(row))
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            set_pixel(rgba, width, left + x, top + y, spectral_color(value));
        }
    }
}

fn draw_midi(
    rgba: &mut [u8],
    width: usize,
    height: usize,
    notes: &[crate::MidiChipNote],
) {
    let left = 14_f32;
    let top = 14_f32;
    let usable_w = width.saturating_sub(28) as f32;
    let usable_h = height.saturating_sub(28) as f32;
    if notes.is_empty() {
        // Empty selection: quiet piano-roll frame only.
        hline(
            rgba,
            width,
            height,
            left as usize,
            (left + usable_w) as usize,
            (top + usable_h * 0.5) as usize,
            [55, 72, 92, 120],
        );
        return;
    }

    const MAX_DRAW: usize = 96;
    for note in notes.iter().take(MAX_DRAW) {
        let start = note.start.clamp(0.0, 1.0);
        let end = note.end.clamp(0.0, 1.0).max(start + 0.01);
        let pitch = note.pitch.clamp(0.0, 1.0);
        let x = left + start * usable_w;
        let note_w = ((end - start) * usable_w).max(2.0);
        let note_h = (usable_h / 18.0).clamp(3.0, 7.0);
        // pitch 0 = low (bottom), 1 = high (top)
        let y = top + (1.0 - pitch) * (usable_h - note_h);
        fill_rounded(
            rgba,
            width,
            height,
            x.round() as usize,
            y.round() as usize,
            note_w.round() as usize,
            note_h.round() as usize,
            2,
            [120, 210, 190, 230],
        );
    }
}

fn spectral_color(value: f32) -> [u8; 4] {
    let cold = [45.0, 54.0, 82.0];
    let mid = [49.0, 180.0, 178.0];
    let hot = [247.0, 214.0, 112.0];
    let (a, b, t) = if value < 0.55 {
        (cold, mid, value / 0.55)
    } else {
        (mid, hot, (value - 0.55) / 0.45)
    };
    [
        (a[0] + (b[0] - a[0]) * t).round() as u8,
        (a[1] + (b[1] - a[1]) * t).round() as u8,
        (a[2] + (b[2] - a[2]) * t).round() as u8,
        245,
    ]
}

fn clear(rgba: &mut [u8]) {
    rgba.fill(0);
}

fn set_pixel(rgba: &mut [u8], width: usize, x: usize, y: usize, color: [u8; 4]) {
    if x >= width {
        return;
    }
    let index = (y * width + x) * 4;
    if index + 3 >= rgba.len() {
        return;
    }
    // Source-over blend for soft edges.
    let dst = &mut rgba[index..index + 4];
    let src_a = color[3] as f32 / 255.0;
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return;
    }
    for c in 0..3 {
        let s = color[c] as f32;
        let d = dst[c] as f32;
        dst[c] = ((s * src_a + d * dst_a * (1.0 - src_a)) / out_a).round() as u8;
    }
    dst[3] = (out_a * 255.0).round() as u8;
}

fn hline(rgba: &mut [u8], width: usize, height: usize, x0: usize, x1: usize, y: usize, color: [u8; 4]) {
    if y >= height {
        return;
    }
    let (a, b) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
    for x in a..=b.min(width.saturating_sub(1)) {
        set_pixel(rgba, width, x, y, color);
    }
}

fn vbar(
    rgba: &mut [u8],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    bar_w: usize,
    bar_h: usize,
    color: [u8; 4],
) {
    for dy in 0..bar_h {
        let py = y + dy;
        if py >= height {
            break;
        }
        for dx in 0..bar_w {
            set_pixel(rgba, width, x + dx, py, color);
        }
    }
}

fn fill_rounded(
    rgba: &mut [u8],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    radius: usize,
    color: [u8; 4],
) {
    if w == 0 || h == 0 {
        return;
    }
    let r = radius.min(w / 2).min(h / 2);
    for py in y..y.saturating_add(h).min(height) {
        for px in x..x.saturating_add(w).min(width) {
            if inside_rounded(px, py, x, y, w, h, r) {
                set_pixel(rgba, width, px, py, color);
            }
        }
    }
}

fn stroke_rounded(
    rgba: &mut [u8],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    radius: usize,
    color: [u8; 4],
) {
    if w == 0 || h == 0 {
        return;
    }
    let r = radius.min(w / 2).min(h / 2);
    for py in y..y.saturating_add(h).min(height) {
        for px in x..x.saturating_add(w).min(width) {
            let inside = inside_rounded(px, py, x, y, w, h, r);
            let inset = inside_rounded(px, py, x + 1, y + 1, w.saturating_sub(2), h.saturating_sub(2), r.saturating_sub(1));
            if inside && !inset {
                set_pixel(rgba, width, px, py, color);
            }
        }
    }
}

fn inside_rounded(px: usize, py: usize, x: usize, y: usize, w: usize, h: usize, r: usize) -> bool {
    if px < x || py < y || px >= x + w || py >= y + h {
        return false;
    }
    if r == 0 {
        return true;
    }
    let lx = px - x;
    let ly = py - y;
    let rx = (x + w - 1).saturating_sub(px);
    let ry = (y + h - 1).saturating_sub(py);
    let (cx, cy) = if lx < r && ly < r {
        (r - lx, r - ly)
    } else if rx < r && ly < r {
        (r - rx, r - ly)
    } else if lx < r && ry < r {
        (r - lx, r - ry)
    } else if rx < r && ry < r {
        (r - rx, r - ry)
    } else {
        return true;
    };
    cx * cx + cy * cy <= r * r
}

/// Convert straight RGBA to BGRA (X11 ZPixmap / Windows DIB).
#[must_use]
pub fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
    }
    out
}

/// Convert straight RGBA to premultiplied ARGB8888 (Wayland wl_shm).
#[must_use]
pub fn rgba_to_argb8888_premul(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        let a = chunk[3] as u32;
        let r = (chunk[0] as u32 * a / 255) as u8;
        let g = (chunk[1] as u32 * a / 255) as u8;
        let b = (chunk[2] as u32 * a / 255) as u8;
        out.extend_from_slice(&[b, g, r, chunk[3]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MidiChipNote;

    #[test]
    fn waveform_chip_has_non_transparent_pixels() {
        let buckets: Vec<(f32, f32)> = (0..64)
            .map(|i| {
                let t = i as f32 / 63.0;
                (-t * 0.6, t * 0.8)
            })
            .collect();
        let image = render_drag_chip(&ExternalDragPreview::Waveform { buckets });
        assert_eq!(image.width, CHIP_WIDTH);
        assert_eq!(image.height, CHIP_HEIGHT);
        let opaque = image.rgba.chunks_exact(4).filter(|p| p[3] > 40).count();
        assert!(opaque > 500, "expected filled card, got {opaque} opaque pixels");
    }

    #[test]
    fn midi_chip_renders_real_notes() {
        let image = render_drag_chip(&ExternalDragPreview::Midi {
            notes: vec![
                MidiChipNote {
                    start: 0.1,
                    end: 0.4,
                    pitch: 0.2,
                },
                MidiChipNote {
                    start: 0.5,
                    end: 0.8,
                    pitch: 0.8,
                },
            ],
        });
        let opaque = image.rgba.chunks_exact(4).filter(|p| p[3] > 40).count();
        assert!(opaque > 500);
    }

    #[test]
    fn spectral_uses_column_major_layout() {
        // One hot cell at column 0, high row (top of spectrogram after Y flip).
        let columns = 4;
        let rows = 4;
        let mut energy = vec![0.0_f32; columns * rows];
        energy[0 * rows + (rows - 1)] = 1.0; // col0, high freq
        let image = render_drag_chip(&ExternalDragPreview::Spectral {
            columns,
            rows,
            energy,
            low_hz: 20.0,
            high_hz: 8_000.0,
        });
        // Sample near top-left of inner panel — should be hot (gold-ish), not cold.
        let left = 14;
        let top = 14;
        let i = ((top + 2) * image.width + (left + 2)) * 4;
        let r = image.rgba[i];
        let g = image.rgba[i + 1];
        let b = image.rgba[i + 2];
        assert!(
            r > 100 || g > 100,
            "expected hot spectral color at high-freq col0, got rgb({r},{g},{b})"
        );
    }
}
