//! 한 변이 egui `max_texture_side`를 넘는 그림을 여러 텍스처로 나눠 올리고 그린다.
//! egui는 한 변이 그 값을 넘는 텍스처를 받으면 debug 빌드에서 패닉한다. 이미지 뷰어는
//! 확대가 핵심이라 줄여 올리지 않고 원본 픽셀을 타일로 나눈다.
//!
//! 이웃 타일과 맞닿는 변에는 1px 테두리를 더 담아 올리고 그 테두리는 그리지 않는다.
//! 선형 보간이 경계에서도 실제 이웃 픽셀을 섞으므로 확대해도 이음매가 보이지 않는다.

use egui::{ColorImage, Pos2, Rect, TextureHandle, TextureOptions};

/// 한 축의 타일 하나. `start..start + len`이 그리는 구간, `tex_start..tex_start + tex_len`이
/// 텍스처에 담는 구간이다(그리는 구간 + 이웃 쪽 테두리).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: usize,
    len: usize,
    tex_start: usize,
    tex_len: usize,
}

/// 길이 `len`을 텍스처 한 변 `max_side` 이하의 구간으로 나눈다.
fn spans(len: usize, max_side: usize) -> Vec<Span> {
    let max_side = max_side.max(1);
    if len <= max_side {
        return vec![Span {
            start: 0,
            len,
            tex_start: 0,
            tex_len: len,
        }];
    }
    // 양쪽 테두리를 담을 자리가 없으면 겹침 없이 나눈다.
    let border = usize::from(max_side > 2);
    let step = max_side - 2 * border;
    let mut out = Vec::with_capacity(len.div_ceil(step));
    let mut start = 0;
    while start < len {
        let span_len = step.min(len - start);
        let tex_start = start.saturating_sub(border);
        let tex_end = (start + span_len + border).min(len);
        out.push(Span {
            start,
            len: span_len,
            tex_start,
            tex_len: tex_end - tex_start,
        });
        start += span_len;
    }
    out
}

struct Tile {
    texture: TextureHandle,
    x: Span,
    y: Span,
}

/// 타일로 나눠 올린 그림 하나.
pub struct TiledTexture {
    size: [usize; 2],
    tiles: Vec<Tile>,
}

impl TiledTexture {
    /// `image`를 `ctx`의 `max_texture_side` 이하 타일로 올린다.
    pub fn load(
        ctx: &egui::Context,
        name: &str,
        image: &ColorImage,
        options: TextureOptions,
    ) -> Self {
        let max_side = ctx.input(|i| i.max_texture_side);
        let [w, h] = image.size;
        let xs = spans(w, max_side);
        let ys = spans(h, max_side);
        let single = xs.len() == 1 && ys.len() == 1;
        let mut tiles = Vec::with_capacity(xs.len() * ys.len());
        for y in &ys {
            for x in &xs {
                let texture = if single {
                    ctx.load_texture(name, image.clone(), options)
                } else {
                    let tile_name = format!("{name}[{},{}]", x.start, y.start);
                    ctx.load_texture(tile_name, crop(image, *x, *y), options)
                };
                tiles.push(Tile {
                    texture,
                    x: *x,
                    y: *y,
                });
            }
        }
        Self {
            size: image.size,
            tiles,
        }
    }

    /// 그림 전체가 `rect`를 채우도록 그린다.
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, tint: egui::Color32) {
        for (screen, uv, tile) in self.placements(rect) {
            painter.image(tile.texture.id(), screen, uv, tint);
        }
    }

    fn placements(&self, rect: Rect) -> impl Iterator<Item = (Rect, Rect, &Tile)> {
        let [w, h] = self.size;
        let sx = rect.width() / w.max(1) as f32;
        let sy = rect.height() / h.max(1) as f32;
        self.tiles.iter().map(move |tile| {
            let screen = Rect::from_min_max(
                Pos2::new(
                    rect.min.x + tile.x.start as f32 * sx,
                    rect.min.y + tile.y.start as f32 * sy,
                ),
                Pos2::new(
                    rect.min.x + (tile.x.start + tile.x.len) as f32 * sx,
                    rect.min.y + (tile.y.start + tile.y.len) as f32 * sy,
                ),
            );
            (screen, uv_of(tile.x, tile.y), tile)
        })
    }
}

/// 텍스처 안에서 그리는 구간의 uv. 테두리는 uv 밖에 남는다.
fn uv_of(x: Span, y: Span) -> Rect {
    let axis = |s: Span| {
        let lo = (s.start - s.tex_start) as f32 / s.tex_len as f32;
        let hi = (s.start + s.len - s.tex_start) as f32 / s.tex_len as f32;
        (lo, hi)
    };
    let (u0, u1) = axis(x);
    let (v0, v1) = axis(y);
    Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u1, v1))
}

fn crop(image: &ColorImage, x: Span, y: Span) -> ColorImage {
    let [w, _] = image.size;
    let mut pixels = Vec::with_capacity(x.tex_len * y.tex_len);
    for row in y.tex_start..y.tex_start + y.tex_len {
        let at = row * w + x.tex_start;
        pixels.extend_from_slice(&image.pixels[at..at + x.tex_len]);
    }
    ColorImage {
        size: [x.tex_len, y.tex_len],
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_side(side: usize) -> egui::Context {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            max_texture_side: Some(side),
            ..Default::default()
        };
        let _output = ctx.run(input, |_| {});
        ctx
    }

    /// 픽셀마다 다른 값을 가진 그림. 타일이 원본 픽셀을 옮겼는지 비교하는 데 쓴다.
    fn gradient(w: usize, h: usize) -> ColorImage {
        let bytes: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % w) as u8, (i / w) as u8, (i % 7) as u8, 255])
            .collect();
        ColorImage::from_rgba_unmultiplied([w, h], &bytes)
    }

    /// 나눈 구간은 빈틈 없이 이어지고, 텍스처 구간은 한계 안에서 그리는 구간을 포함한다.
    #[test]
    fn spans_cover_the_length_without_exceeding_the_side() {
        for (len, side) in [
            (9000, 2048),
            (4000, 2048),
            (2049, 2048),
            (2048, 2048),
            (16, 2048),
            (7, 3),
            (5, 2),
            (0, 2048),
        ] {
            let s = spans(len, side);
            let mut next = 0;
            for span in &s {
                assert_eq!(span.start, next, "len {len} side {side}");
                assert!(span.tex_len <= side, "len {len} side {side}: {span:?}");
                assert!(span.tex_start <= span.start);
                assert!(span.start + span.len <= span.tex_start + span.tex_len);
                assert!(span.tex_start + span.tex_len <= len);
                next = span.start + span.len;
            }
            assert_eq!(next, len, "len {len} side {side}");
        }
    }

    /// 이웃 타일 쪽에는 1px 테두리를 담는다.
    #[test]
    fn inner_edges_carry_a_one_pixel_border() {
        let s = spans(4000, 2048);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].tex_start, s[0].tex_len), (0, 2047));
        assert_eq!(s[1].tex_start, s[0].start + s[0].len - 1);
    }

    /// 넓은 그림·극단 비율도 패닉 없이 올라가고 각 타일이 한계 안에 들며 원본 픽셀을 그대로 담는다.
    #[test]
    fn large_and_extreme_images_load_under_the_side() {
        let side = 2048;
        let ctx = ctx_with_side(side);
        for (w, h) in [
            (4000, 3000),
            (9000, 16),
            (16, 9000),
            (2048, 2048),
            (300, 200),
        ] {
            let image = gradient(w, h);
            let tiled = TiledTexture::load(&ctx, "t", &image, TextureOptions::LINEAR);
            let mut covered = 0;
            for tile in &tiled.tiles {
                let [tw, th] = tile.texture.size();
                assert!(tw <= side && th <= side, "{w}x{h}: tile {tw}x{th}");
                assert_eq!([tw, th], [tile.x.tex_len, tile.y.tex_len]);
                covered += tile.x.len * tile.y.len;
            }
            assert_eq!(covered, w * h, "{w}x{h}");
        }
        let image = gradient(9000, 16);
        let x = spans(9000, side)[2];
        let y = spans(16, side)[0];
        let piece = crop(&image, x, y);
        assert_eq!(piece.pixels[0], image.pixels[x.tex_start]);
        assert_eq!(
            piece.pixels[x.tex_len + 3],
            image.pixels[9000 + x.tex_start + 3]
        );
    }

    /// 타일 화면 사각형은 받은 사각형을 빈틈·겹침 없이 채우고, uv는 테두리를 뺀 구간이다.
    #[test]
    fn placements_tile_the_target_rect() {
        let ctx = ctx_with_side(2048);
        let image = gradient(4000, 3000);
        let tiled = TiledTexture::load(&ctx, "t", &image, TextureOptions::LINEAR);
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(400.0, 300.0));
        let mut area = 0.0;
        for (screen, uv, _) in tiled.placements(rect) {
            assert!(rect.expand(1e-3).contains_rect(screen));
            area += screen.area();
            assert!(uv.min.x >= 0.0 && uv.max.x <= 1.0 && uv.min.y >= 0.0 && uv.max.y <= 1.0);
        }
        assert!((area - rect.area()).abs() < 1e-2, "{area}");
        let small = TiledTexture::load(&ctx, "s", &gradient(30, 20), TextureOptions::LINEAR);
        let (_, uv, _) = small.placements(rect).next().expect("one tile");
        assert_eq!(uv, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)));
    }
}
