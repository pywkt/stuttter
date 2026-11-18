//! Dev tool: renders the editor headlessly to a PPM image so the layout can
//! be looked at without a host. Runs only when `STUTTTER_PREVIEW` names
//! an output directory.

use std::collections::HashMap;
use std::sync::Arc;

use egui::epaint::{ImageData, Primitive};
use egui::{Color32, TextureId};

use super::StuttterEditor;
use crate::StuttterParams;
use crate::actions::SpeedPreset;
use crate::pattern::{Action, NoteValue, Source};
use crate::scope::Scope;

type Textures = HashMap<TextureId, egui::ColorImage>;

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Canvas {
        Canvas { width, height, pixels: vec![[0.0, 0.0, 0.0, 1.0]; width * height] }
    }

    fn sample(textures: &Textures, id: TextureId, uv: egui::Pos2) -> [f32; 4] {
        let Some(image) = textures.get(&id) else {
            return [1.0; 4];
        };
        let x = ((uv.x * image.size[0] as f32) as usize).min(image.size[0] - 1);
        let y = ((uv.y * image.size[1] as f32) as usize).min(image.size[1] - 1);
        let c = image.pixels[y * image.size[0] + x];
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
    }

    fn triangle(
        &mut self,
        textures: &Textures,
        id: TextureId,
        v: [&egui::epaint::Vertex; 3],
        clip: egui::Rect,
    ) {
        let (p0, p1, p2) = (v[0].pos, v[1].pos, v[2].pos);
        let area = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
        if area.abs() < 1e-9 {
            return;
        }
        let min_x = p0.x.min(p1.x).min(p2.x).max(clip.min.x).max(0.0).floor() as usize;
        let right = clip.max.x.min(self.width as f32);
        let max_x = p0.x.max(p1.x).max(p2.x).min(right).ceil() as usize;
        let min_y = p0.y.min(p1.y).min(p2.y).max(clip.min.y).max(0.0).floor() as usize;
        let bottom = clip.max.y.min(self.height as f32);
        let max_y = p0.y.max(p1.y).max(p2.y).min(bottom).ceil() as usize;
        for y in min_y..max_y {
            for x in min_x..max_x {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((p1.x - px) * (p2.y - py) - (p2.x - px) * (p1.y - py)) / area;
                let w1 = ((p2.x - px) * (p0.y - py) - (p0.x - px) * (p2.y - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let lerp = |a: f32, b: f32, c: f32| a * w0 + b * w1 + c * w2;
                let uv = egui::pos2(
                    lerp(v[0].uv.x, v[1].uv.x, v[2].uv.x),
                    lerp(v[0].uv.y, v[1].uv.y, v[2].uv.y),
                );
                let colour = |c: Color32| [c.r(), c.g(), c.b(), c.a()].map(|v| v as f32 / 255.0);
                let (c0, c1, c2) = (colour(v[0].color), colour(v[1].color), colour(v[2].color));
                let tex = Canvas::sample(textures, id, uv);
                let mut src = [0.0; 4];
                for i in 0..4 {
                    src[i] = lerp(c0[i], c1[i], c2[i]) * tex[i];
                }
                let dst = &mut self.pixels[y * self.width + x];
                for i in 0..3 {
                    dst[i] = src[i] + dst[i] * (1.0 - src[3]);
                }
            }
        }
    }

    fn save_ppm(&self, path: &std::path::Path) {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for p in &self.pixels {
            for c in &p[..3] {
                out.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        std::fs::write(path, out).expect("write preview");
    }
}

fn render(editor: &mut StuttterEditor, width: f32, height: f32, name: &str, out: &str) {
    let ctx = egui::Context::default();
    super::theme::apply(&ctx);
    let mut textures = Textures::new();
    let mut last = None;
    for _ in 0..4 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, (width, height).into())),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| editor.draw(ui));
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
            let ImageData::Color(image) = &delta.image;
            match delta.pos {
                None => {
                    textures.insert(*id, image.as_ref().clone());
                }
                Some([x0, y0]) => {
                    let target = textures.get_mut(id).expect("patch of a known texture");
                    for y in 0..image.size[1] {
                        for x in 0..image.size[0] {
                            target.pixels[(y0 + y) * target.size[0] + x0 + x] =
                                image.pixels[y * image.size[0] + x];
                        }
                    }
                }
            }
            }
        }
        output.textures_delta.clear();
        last = Some(output);
    }
    let output = last.expect("a frame");
    let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
    let mut canvas = Canvas::new(width as usize, height as usize);
    for primitive in primitives {
        if let Primitive::Mesh(mesh) = primitive.primitive {
            for tri in mesh.indices.chunks(3) {
                let v = [
                    &mesh.vertices[tri[0] as usize],
                    &mesh.vertices[tri[1] as usize],
                    &mesh.vertices[tri[2] as usize],
                ];
                canvas.triangle(&textures, mesh.texture_id, v, primitive.clip_rect);
            }
        }
    }
    canvas.save_ppm(&std::path::Path::new(out).join(format!("{name}.ppm")));
}

#[test]
fn render_the_editor() {
    let Ok(out) = std::env::var("STUTTTER_PREVIEW") else {
        return;
    };
    let params = Arc::new(StuttterParams::default());
    let scope = Arc::new(Scope::new());
    for i in 0..44_100 {
        let phase = i as f64 / 44_100.0;
        let t = i as f32 / 300.0;
        let x = (t.sin() * 0.6) * (1.0 - (phase as f32 * 4.0).fract());
        scope.write(phase, (x, x), (x * 0.8, x * 0.8));
    }
    let model = params.state.model();
    let stop = Action::from_preset(SpeedPreset::TapeStop).with_source(Source::Continue);
    model.add_block(0, 0, 12, Action::repeat(NoteValue::Quarter));
    model.add_block(0, 12, 4, stop);
    let rising = Action { from: 1.0, to: 2.0, ..Action::repeat(NoteValue::Sixteenth) };
    model.add_block(1, 4, 8, rising.with_source(Source::Spot { step: 2, passes: 0 }));
    model.set_volume(0, 3, 0.4);
    // The user's case: a beat from the start brought back at the end, with
    // other blocks free in between.
    let spot = Source::Spot { step: 0, passes: 0 };
    model.add_block(2, 12, 4, Action::repeat(NoteValue::Sixteenth).with_source(spot));
    model.add_block(2, 4, 2, Action::repeat(NoteValue::ThirtySecond));
    model.add_block(2, 8, 2, Action::from_preset(SpeedPreset::Reverse));

    // Pattern 1 playing (from a note), so the waveform is of the grid on
    // screen and the output takes its blocks' colours.
    params.state.prefs().set_notes(Some(84));
    params.note_pattern.store(1, std::sync::atomic::Ordering::Relaxed);

    let mut editor = StuttterEditor::new(Arc::clone(&params), Arc::clone(&scope));
    render(&mut editor, super::DEFAULT_WIDTH, super::DEFAULT_HEIGHT, "empty-selection", &out);
    editor.row.editor.select(model, 0, Some(0));
    render(&mut editor, super::DEFAULT_WIDTH, super::DEFAULT_HEIGHT, "repeat-selected", &out);
    editor.row.editor.select(model, 0, Some(1));
    render(&mut editor, super::DEFAULT_WIDTH, super::DEFAULT_HEIGHT, "stop-selected", &out);
    editor.row.editor.select(model, 0, Some(0));
    render(&mut editor, super::MIN_WIDTH, super::MIN_HEIGHT, "minimum-size", &out);
    render(&mut editor, 1300.0, 700.0, "large", &out);
    editor.patterns.select(1);
    editor.row.editor.select(model, 1, Some(0));
    render(&mut editor, super::DEFAULT_WIDTH, super::DEFAULT_HEIGHT, "before-selected", &out);
    editor.patterns.select(2);
    editor.row.editor.select(model, 2, Some(2));
    render(&mut editor, super::DEFAULT_WIDTH, super::DEFAULT_HEIGHT, "spot-selected", &out);
    // A low-pass sweeping down across pattern 3, shown on its Cutoff row.
    model.choose_filter(2, crate::pattern::Filter::LowPass);
    let sweep = (0..16).map(|step| 1.0 - step as f32 / 20.0);
    model.set_lane(2, crate::pattern::Lane::Cutoff, sweep);
    editor.lane = crate::pattern::Lane::Cutoff;
    render(&mut editor, super::DEFAULT_WIDTH, super::DEFAULT_HEIGHT, "cutoff-row", &out);
    // A starter pattern, its first block selected so the panel describes it.
    model.set_rate(3, crate::pattern::Rate::Eighth);
    model.set_length(3, 32);
    model.fill_pattern(3, crate::starters::Starter::ReverseSnares);
    editor.lane = crate::pattern::Lane::Volume;
    editor.patterns.select(3);
    editor.row.editor.select(model, 3, Some(0));
    render(&mut editor, super::DEFAULT_WIDTH, super::DEFAULT_HEIGHT, "starter", &out);
}
