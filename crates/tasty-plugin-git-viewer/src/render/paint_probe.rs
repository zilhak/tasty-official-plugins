//! 렌더 시험 도우미. 한 프레임의 출력 도형을 칠하는 순서대로 하나씩 테셀레이트하고,
//! 글자 영역 안의 점마다 그 점을 덮는 단색 삼각형의 색을 칠한 순서대로 쌓는다.
//! 도형 종류와 무관하게 실제로 칠해지는 삼각형을 보므로 사각형이 아닌 채움, 알파 0 가산 채움,
//! 다른 채움에 가려진 채움도 스택에 드러난다.

use egui::epaint::{ClippedPrimitive, ClippedShape, Primitive, WHITE_UV};
use egui::{Color32, Pos2, Rect, Shape, TextureId, vec2};

/// 시험용 Context. feathering과 미리 래스터한 원을 꺼서 단색 채움이 글꼴 텍스처의
/// `WHITE_UV` 단색 삼각형으로만 나오게 한다. 글자 테셀레이션은 feathering과 무관하다.
pub(super) fn probe_context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.tessellation_options_mut(|o| {
        o.feathering = false;
        o.prerasterized_discs = false;
    });
    ctx
}

/// 출력에 그려진 글자 도형 하나.
pub(super) struct PaintedText {
    pub(super) text: String,
    pub(super) rect: Rect,
    /// 실제 테셀레이터가 낸 정점 색의 종류. galley 색, override·fallback, 도형 opacity가 모두 반영된다.
    /// 밑줄·배경이 없는 글자 도형이라 정점은 모두 글리프다.
    pub(super) glyph_colors: Vec<Color32>,
    pub(super) opacity_factor: f32,
}

/// 글자가 아닌 도형 하나를 따로 테셀레이트한 결과. `index`는 출력 순서의 도형 번호다.
struct PaintedShape {
    index: usize,
    prims: Vec<ClippedPrimitive>,
}

/// 한 프레임에서 모은 글자 도형, 칠하는 순서의 도형별 테셀레이션, 사각형 채움.
pub(super) struct Painted {
    pub(super) texts: Vec<PaintedText>,
    shapes: Vec<PaintedShape>,
    /// `Shape::Rect`의 (사각형 ∩ 클립, 채움). 보완 검사가 쓴다.
    rects: Vec<(Rect, Color32)>,
    ppp: f32,
}

fn flatten(clip: Rect, shape: &Shape, out: &mut Vec<(Rect, Shape)>) {
    match shape {
        Shape::Vec(v) => v.iter().for_each(|s| flatten(clip, s, out)),
        s => out.push((clip, s.clone())),
    }
}

/// `Shape::Vec`을 순서대로 펼친 뒤 글자 도형을 떼고, 나머지는 도형마다 따로 테셀레이트한다.
pub(super) fn collect(ctx: &egui::Context, out: &egui::FullOutput) -> Painted {
    let ppp = out.pixels_per_point;
    let mut flat = Vec::new();
    for clipped in &out.shapes {
        flatten(clipped.clip_rect, &clipped.shape, &mut flat);
    }
    let mut painted = Painted {
        texts: Vec::new(),
        shapes: Vec::new(),
        rects: Vec::new(),
        ppp,
    };
    for (index, (clip_rect, shape)) in flat.into_iter().enumerate() {
        if let Shape::Rect(r) = &shape {
            // 화면에 칠해지는 영역은 사각형과 클립의 교차다.
            painted.rects.push((r.rect.intersect(clip_rect), r.fill));
        }
        let prims = ctx.tessellate(
            vec![ClippedShape {
                clip_rect,
                shape: shape.clone(),
            }],
            ppp,
        );
        if let Shape::Text(t) = &shape {
            let mut glyph_colors: Vec<Color32> = Vec::new();
            for prim in &prims {
                if let Primitive::Mesh(mesh) = &prim.primitive {
                    for v in &mesh.vertices {
                        if !glyph_colors.contains(&v.color) {
                            glyph_colors.push(v.color);
                        }
                    }
                }
            }
            painted.texts.push(PaintedText {
                text: t.galley.text().to_owned(),
                rect: t.galley.rect.translate(t.pos.to_vec2()),
                glyph_colors,
                opacity_factor: t.opacity_factor,
            });
        } else {
            painted.shapes.push(PaintedShape { index, prims });
        }
    }
    painted
}

/// 탐침 영역 다섯 곳. 본문 글자 rect 안쪽의 네 모서리 근처와 중앙이다.
const REGIONS: [&str; 5] = ["좌상", "우상", "우하", "좌하", "중앙"];
/// 후보 점 사이 간격(pt). 2의 거듭제곱 분수라 f32 좌표가 정확하다.
const STEP: f32 = 1.0 / 16.0;
/// 영역마다 차례로 시도하는 후보 점의 기준점 대비 이동(STEP 단위).
const OFFSETS: [(f32, f32); 8] = [
    (0.0, 0.0),
    (0.0, 1.0),
    (1.0, 0.0),
    (1.0, 1.0),
    (-1.0, 1.0),
    (1.0, -1.0),
    (-1.0, 0.0),
    (0.0, -1.0),
];

fn region_anchor(body: Rect, region: usize) -> Pos2 {
    let d = (body.width().min(body.height()) * 0.25).min(2.0);
    match region {
        0 => body.left_top() + vec2(d, d),
        1 => body.right_top() + vec2(-d, d),
        2 => body.right_bottom() + vec2(-d, -d),
        3 => body.left_bottom() + vec2(d, -d),
        _ => body.center(),
    }
}

/// 점 하나가 삼각형에 대해 놓인 자리.
#[derive(PartialEq)]
enum Cover {
    Outside,
    /// 변이나 꼭짓점 위. 이웃 삼각형과 함께 세면 한 칠하기를 두 번 셀 수 있다.
    Boundary,
    /// 내부에 엄밀히 든다.
    Inside,
}

/// `a→b`에 대한 `p`의 방향 행렬식. 반올림 오차보다 작으면 0으로 본다.
fn orient(a: Pos2, b: Pos2, p: Pos2) -> f64 {
    let (ax, ay) = (f64::from(a.x), f64::from(a.y));
    let l = (f64::from(b.x) - ax) * (f64::from(p.y) - ay);
    let r = (f64::from(b.y) - ay) * (f64::from(p.x) - ax);
    let d = l - r;
    if d.abs() <= (l.abs() + r.abs()) * 1e-12 {
        0.0
    } else {
        d
    }
}

fn cover(t: [Pos2; 3], p: Pos2) -> Cover {
    let d = [
        orient(t[0], t[1], p),
        orient(t[1], t[2], p),
        orient(t[2], t[0], p),
    ];
    let neg = d.iter().any(|v| *v < 0.0);
    let pos = d.iter().any(|v| *v > 0.0);
    if neg && pos {
        return Cover::Outside;
    }
    if d.iter().all(|v| *v != 0.0) {
        return Cover::Inside;
    }
    if !neg && !pos {
        // 넓이 0 삼각형과 한 직선 위: 그 선분의 범위 안에 있을 때만 걸친다.
        let (min_x, max_x) = (
            t[0].x.min(t[1].x).min(t[2].x),
            t[0].x.max(t[1].x).max(t[2].x),
        );
        let (min_y, max_y) = (
            t[0].y.min(t[1].y).min(t[2].y),
            t[0].y.max(t[1].y).max(t[2].y),
        );
        if p.x < min_x || p.x > max_x || p.y < min_y || p.y > max_y {
            return Cover::Outside;
        }
    }
    Cover::Boundary
}

fn on_clip_edge(clip: Rect, p: Pos2) -> bool {
    clip.contains(p)
        && (p.x == clip.min.x || p.x == clip.max.x || p.y == clip.min.y || p.y == clip.max.y)
}

/// 색으로만 칠하는 삼각형인가: 글꼴 텍스처의 `WHITE_UV`이고 세 정점 색이 같다.
fn flat_color(mesh: &egui::epaint::Mesh, tri: [usize; 3]) -> Option<Color32> {
    let v = tri.map(|i| mesh.vertices[i]);
    let flat = mesh.texture_id == TextureId::Managed(0)
        && v.iter().all(|v| v.uv == WHITE_UV)
        && v.iter().all(|x| x.color == v[0].color);
    flat.then_some(v[0].color)
}

fn triangles(mesh: &egui::epaint::Mesh) -> impl Iterator<Item = [usize; 3]> + '_ {
    mesh.indices
        .chunks_exact(3)
        .map(|c| [c[0] as usize, c[1] as usize, c[2] as usize])
}

/// 영역 하나에서 고른 탐침 점과 그 점의 채움 스택.
pub(super) struct Probe {
    pub(super) region: &'static str,
    /// 고른 후보의 순번(0이 첫 후보).
    pub(super) candidate: usize,
    pub(super) point: Pos2,
    /// 칠한 순서의 (색, 도형 번호). 마지막 불투명 층 앞은 버렸다.
    pub(super) layers: Vec<(Color32, usize)>,
}

impl Painted {
    /// 글자가 `s`인 글자 도형. 정확히 하나여야 한다.
    pub(super) fn text(&self, name: &str, s: &str) -> &PaintedText {
        let hits: Vec<&PaintedText> = self.texts.iter().filter(|t| t.text == s).collect();
        assert_eq!(hits.len(), 1, "{name}: '{s}' 글자 도형 수");
        hits[0]
    }

    /// 후보 점이 어떤 삼각형의 변·꼭짓점이나 그 삼각형이 든 클립의 경계 위에 놓였는가.
    fn ambiguous(&self, p: Pos2) -> bool {
        self.shapes.iter().flat_map(|s| &s.prims).any(|prim| {
            let Primitive::Mesh(mesh) = &prim.primitive else {
                return false;
            };
            if !prim.clip_rect.contains(p) {
                return false;
            }
            triangles(mesh).any(|tri| {
                let c = cover(tri.map(|i| mesh.vertices[i].pos), p);
                c == Cover::Boundary || (c == Cover::Inside && on_clip_edge(prim.clip_rect, p))
            })
        })
    }

    /// 점 `p`를 내부에 엄밀히 품고 그 primitive의 클립 안인 삼각형마다 한 층씩, 칠한 순서대로 쌓는다.
    fn layers_at(&self, ctx_msg: &str, p: Pos2) -> Vec<(Color32, usize)> {
        let mut layers = Vec::new();
        for shape in &self.shapes {
            for prim in &shape.prims {
                if !prim.clip_rect.contains(p) {
                    continue;
                }
                let mesh = match &prim.primitive {
                    Primitive::Mesh(mesh) => mesh,
                    Primitive::Callback(_) => {
                        panic!(
                            "{ctx_msg}: 탐침 {p:?}가 도형 {}의 콜백 클립 안",
                            shape.index
                        )
                    }
                };
                for tri in triangles(mesh) {
                    let pos = tri.map(|i| mesh.vertices[i].pos);
                    if cover(pos, p) != Cover::Inside {
                        continue;
                    }
                    let Some(color) = flat_color(mesh, tri) else {
                        panic!(
                            "{ctx_msg}: 탐침 {p:?}를 덮는 도형 {}의 삼각형 {pos:?}가 단색이 아니다 \
                             (texture {:?}, uv {:?}, 색 {:?})",
                            shape.index,
                            mesh.texture_id,
                            tri.map(|i| mesh.vertices[i].uv),
                            tri.map(|i| mesh.vertices[i].color),
                        )
                    };
                    // epaint는 정확히 TRANSPARENT인 채움만 건너뛴다. 알파 0이라도 RGB가 있으면 더해진다.
                    if color != Color32::TRANSPARENT {
                        layers.push((color, shape.index));
                    }
                }
            }
        }
        if let Some(last_opaque) = layers.iter().rposition(|(c, _)| c.a() == 255) {
            layers.drain(..last_opaque);
        }
        layers
    }

    /// 본문 rect 안 다섯 영역에서 탐침을 고르고 채움 스택을 잰다.
    /// 후보가 변·꼭짓점·클립 경계 위면 같은 영역의 다음 후보를 쓰고, 모든 후보가 걸리면 실패한다.
    pub(super) fn probes(&self, ctx_msg: &str, body: Rect) -> Vec<Probe> {
        assert!(
            body.width() >= 1.0 && body.height() >= 1.0,
            "{ctx_msg}: 본문 rect {body:?}가 탐침을 두기에 작다"
        );
        (0..REGIONS.len())
            .map(|region| {
                let anchor = region_anchor(body, region);
                let candidates: Vec<Pos2> = OFFSETS
                    .iter()
                    .map(|(dx, dy)| anchor + vec2(dx * STEP, dy * STEP))
                    .collect();
                let Some(candidate) = candidates.iter().position(|p| !self.ambiguous(*p)) else {
                    panic!(
                        "{ctx_msg}: {} 영역의 후보 {candidates:?}가 모두 삼각형 변·꼭짓점이나 클립 경계 위다 \
                         (본문 {body:?}, ppp {})",
                        REGIONS[region], self.ppp
                    )
                };
                let point = candidates[candidate];
                Probe {
                    region: REGIONS[region],
                    candidate,
                    point,
                    layers: self.layers_at(ctx_msg, point),
                }
            })
            .collect()
    }

    /// 다섯 탐침의 채움 스택이 모두 `expected`(칠한 순서)여야 한다.
    pub(super) fn assert_stack(&self, ctx_msg: &str, body: Rect, expected: &[Color32]) {
        for probe in self.probes(ctx_msg, body) {
            // egui의 `Debug`는 소수 한 자리로 줄이므로 좌표를 그대로 적는다.
            let (x, y) = (probe.point.x, probe.point.y);
            println!(
                "{ctx_msg}: {} 탐침 후보 {} ({x}, {y}), 층 {:?}",
                probe.region, probe.candidate, probe.layers
            );
            let colors: Vec<Color32> = probe.layers.iter().map(|(c, _)| *c).collect();
            assert_eq!(
                colors,
                expected,
                "{ctx_msg}: {} 탐침 ({x}, {y})(후보 {})의 채움 스택. 층(색, 도형 번호) {:?}, \
                 본문 ({}, {})-({}, {}), ppp {}",
                probe.region,
                probe.candidate,
                probe.layers,
                body.min.x,
                body.min.y,
                body.max.x,
                body.max.y,
                self.ppp
            );
        }
    }

    /// 바탕 `base`와 `TRANSPARENT`를 뺀 `Shape::Rect` ∩ 클립 가운데 본문과 양의 넓이로 겹치는 채움.
    fn rect_fills_over(&self, body: Rect, base: Color32) -> Vec<(Rect, Color32)> {
        self.rects
            .iter()
            .filter(|(_, fill)| *fill != base && *fill != Color32::TRANSPARENT)
            .filter(|(r, _)| {
                let o = r.intersect(body);
                o.width() > 0.0 && o.height() > 0.0
            })
            .copied()
            .collect()
    }

    /// 보완 검사. 탐침 다섯 점이 보지 못하는 본문 나머지를 `Shape::Rect` 채움으로 본다.
    /// `tint`가 있으면 사각형 채움이 있을 때 정확히 하나, 그 색, 본문 전체 포함이어야 한다
    /// (사각형이 아닌 도형으로 칠했으면 스택 검사만 받는다). `tint`가 없으면 겹치는 채움이 없어야 한다.
    pub(super) fn assert_rect_fill(
        &self,
        ctx_msg: &str,
        body: Rect,
        base: Color32,
        tint: Option<Color32>,
    ) {
        let over = self.rect_fills_over(body, base);
        match tint {
            None => assert!(
                over.is_empty(),
                "{ctx_msg}: 채우지 않는 행에 사각형 채움 {over:?}(본문 {body:?})"
            ),
            Some(_) if over.is_empty() => {}
            Some(tint) => {
                let colors: Vec<Color32> = over.iter().map(|(_, c)| *c).collect();
                assert_eq!(colors, vec![tint], "{ctx_msg}: 사각형 채움 {over:?}");
                assert!(
                    over[0].0.contains_rect(body),
                    "{ctx_msg}: 사각형 채움 {:?}가 본문 {body:?} 전체를 덮는다",
                    over[0].0
                );
            }
        }
    }
}
