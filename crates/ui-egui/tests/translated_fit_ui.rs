//! Translated labels must fit the widgets they are drawn in. Runs the real `FilmcraftApp` under
//! `egui_kittest` in Russian (one of the longest catalogs), visits every workspace, panel, mode
//! and dialog that the menus open, and inspects the painted shapes of each frame:
//!
//! - a translated label wider than the button, tab or pill it is centred in sticks out of it;
//! - a translated label that runs past its clip rectangle is cut off;
//! - two translated labels that overlap each other are unreadable.
//!
//! Set `FILMCRAFT_FIT_REPORT=<file>` to write every finding as JSON lines, and
//! `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render each screen offscreen (`fit-*.png`).

use std::collections::BTreeMap;
use std::sync::mpsc::{Sender, channel};

use egui::epaint::{ClippedShape, Shape};
use egui::{Rect, Vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
    size: Vec2,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    kind: &'static str,
    text: String,
    screen: String,
    detail: String,
}

impl Driver {
    fn demo(size: Vec2) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(size).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots, size };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        json!({"ok": false, "error": "no reply"})
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        std::fs::create_dir_all(&dir).unwrap();
        let file: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let path = dir.join(format!("fit-{file}.png"));
        img.save(&path).unwrap();
    }

    /// Inspect the shapes painted in the last frame.
    fn inspect(&mut self, screen: &str, out: &mut BTreeMap<(String, &'static str), Finding>) {
        self.frames(3);
        // shapes in paint order: (index, text rect, clip, text) and (index, filled rect, opaque)
        let mut texts = Vec::new();
        let mut fills = Vec::new();
        fn walk(shape: &Shape, clip: Rect, n: &mut usize, texts: &mut Vec<(usize, Rect, Rect, String, bool)>, fills: &mut Vec<(usize, Rect, bool, Rect)>) {
            *n += 1;
            match shape {
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, n, texts, fills)),
                Shape::Text(t) => {
                    let s = t.galley.text().to_string();
                    if !s.trim().is_empty() {
                        texts.push((*n, t.visual_bounding_rect(), clip, s, t.galley.elided));
                    }
                }
                Shape::Rect(r) if r.fill.a() > 0 => fills.push((*n, r.rect.intersect(clip), r.fill.a() == 255, clip)),
                _ => {}
            }
        }
        let mut n = 0;
        for ClippedShape { clip_rect, shape } in &self.harness.output().shapes {
            walk(shape, *clip_rect, &mut n, &mut texts, &mut fills);
        }
        let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, self.size);
        let covered = |a: usize, b: usize, area: Rect| fills.iter().any(|(k, f, opaque, _)| *opaque && *k > a && *k < b && f.contains_rect(area));
        let mut add = |kind: &'static str, text: &str, detail: String| {
            out.entry((text.to_string(), kind)).or_insert(Finding { kind, text: text.to_string(), screen: screen.to_string(), detail });
        };
        for (i, r, clip, s, elided) in &texts {
            if !cyr(s) || !clip.intersects(*r) || !screen_rect.contains(r.center()) {
                continue;
            }
            // hidden under something painted later (a dialog over a panel)
            if covered(*i, usize::MAX, r.intersect(*clip)) {
                continue;
            }
            // partly hidden by a neighbour in the same panel painted after it (the next card, a field)
            if let Some((_, f, _, _)) = fills.iter().find(|(k, f, opaque, fclip)| {
                *opaque
                    && k > i
                    && fclip == clip
                    && f.width() < 600.0
                    && f.height() < 60.0
                    && f.intersects(r.intersect(*clip))
                    && !f.contains_rect(*r)
                    && f.intersect(*r).width() > 3.0
                    && f.intersect(*r).height() > r.height() * 0.5
            }) {
                add("cut off", s, format!("by a box at x {:.0}–{:.0}", f.min.x, f.max.x));
            }
            // cut with "…" by the widget (a label that truncates, a fitted text at its smallest size)
            // (keyboard keycaps abbreviate command names by design, in every language)
            // (and a label that ends in a file name is as long as the user's file)
            let file_name = s.rsplit_once('.').is_some_and(|(_, ext)| (2..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()));
            if *elided && !screen.contains("keyboardShortcuts") && !file_name {
                add("elided", s, format!("{:.0} px", r.width()));
            }
            let visible = r.intersect(*clip);
            if visible.width() > 4.0 && r.width() - visible.width() > 3.0 {
                add("clipped", s, format!("{:.0} of {:.0} px visible", visible.width(), r.width()));
            }
            // the button, pill, tab or field the label sits on: the last filled box painted under it
            let host = fills
                .iter()
                .filter(|(k, b, _, _)| {
                    k < i
                        && !covered(*k, *i, b.intersect(*r))
                        && b.contains(r.center())
                        && b.height() <= 40.0
                        && b.height() >= r.height() * 0.8
                        && b.width() >= 24.0
                        && b.width() < 600.0
                })
                .max_by_key(|(k, _, _, _)| *k);
            if let Some((_, b, _, _)) = host
                && r.width() > b.width() - 4.0
                && (r.min.x < b.min.x - 1.0 || r.max.x > b.max.x + 1.0)
            {
                add("overflows box", s, format!("text {:.0} px, box {:.0} px", r.width(), b.width()));
            }
            for (j, o, oclip, os, _) in &texts {
                if i == j || os == s {
                    continue;
                }
                let a = r.intersect(*clip).intersect(o.intersect(*oclip));
                if a.is_positive() && a.width() > 3.0 && a.height() > r.height().min(o.height()) * 0.5 && !covered(*i.min(j), *i.max(j), a) {
                    add("overlaps", s, format!("with “{os}”"));
                }
            }
        }
        // every screen, except the per-effect ones unless they found something
        if !screen.contains(" fx ") || out.values().any(|f| f.screen == screen) {
            self.snapshot(screen);
        }
    }
}

fn select_first_clip(d: &mut Driver) {
    let seq = d.call("engine.execute", json!({"command": "sequence.inspect", "params": {}}))["result"].clone();
    if let Some(clip) = seq["video"][0]["items"][0]["clip"].as_u64() {
        d.call("engine.execute", json!({"command": "timeline.select", "params": {"clips": [clip]}}));
    }
}

/// The interface language to check (`FILMCRAFT_FIT_LANG`, a menu command suffix; Russian by default).
fn lang() -> String {
    format!("app.language.{}", std::env::var("FILMCRAFT_FIT_LANG").unwrap_or_else(|_| "russian".into()))
}

#[test]
fn russian_labels_fit_their_widgets() {
    let report = std::env::var_os("FILMCRAFT_FIT_REPORT");
    let mut found = BTreeMap::new();
    for size in [egui::vec2(1600.0, 980.0), egui::vec2(1280.0, 800.0)] {
        let tag = format!("{}x{}", size.x, size.y);
        let mut d = Driver::demo(size);
        assert_eq!(d.call("ui.menu.invoke", json!({"id": lang()}))["ok"], json!(true));
        d.frames(4);
        // with a clip selected, so the Clip menu's dialogs are enabled too
        select_first_clip(&mut d);
        let items = d.call("ui.menu.list", json!({}))["result"].as_array().cloned().unwrap_or_default();
        let ws: Vec<String> = items
            .iter()
            .filter(|i| i["path"].as_array().is_some_and(|p| p.iter().any(|x| x == "Workspaces")))
            .filter_map(|i| i["id"].as_str().map(str::to_string))
            .collect();
        d.inspect(&format!("{tag} start"), &mut found);
        for id in ws.iter().filter(|id| !id.ends_with(".edit") && !id.ends_with(".saveAs") && !id.ends_with(".saveChanges") && !id.ends_with(".reset")) {
            if d.call("ui.menu.invoke", json!({"id": id}))["ok"] == json!(true) {
                d.inspect(&format!("{tag} {id}"), &mut found);
            }
        }
        for panel in [
            "Project",
            "MediaBrowser",
            "Libraries",
            "Info",
            "Effects",
            "Markers",
            "History",
            "Source",
            "EffectControls",
            "AudioClipMixer",
            "Metadata",
            "Program",
            "Timeline",
            "Tools",
            "AudioMeters",
            "AudioTrackMixer",
            "LumetriColor",
            "LumetriScopes",
            "EssentialGraphics",
            "EssentialSound",
            "TextToSpeech",
            "Properties",
            "Text",
            "Events",
            "Progress",
            "ReferenceMonitor",
            "Timecode",
            "ProjectNotes",
        ] {
            d.call("ui.panel.show", json!({"panel": panel}));
            d.inspect(&format!("{tag} panel {panel}"), &mut found);
        }
        for mode in ["import", "export", "edit"] {
            d.call("ui.set", json!({"mode": mode}));
            d.inspect(&format!("{tag} mode {mode}"), &mut found);
        }
        // dialogs opened from the menus (skipping ones that open native file pickers or leave the
        // app). Each dialog is closed with Escape; the app is replaced every few dialogs, so a
        // dialog Escape does not close cannot cover many others (a dropped app is not freed in
        // full, so not after every dialog)
        drop(d);
        let skip = [
            "open",
            "import",
            "save",
            "quit",
            "browse",
            "locate",
            "reveal",
            "install",
            "help",
            "report",
            "discord",
            "website",
            "github",
            "getartcraft",
            "close",
            "new.project",
            "language",
            "recover",
            "revert",
            "export.media",
            "media.link",
        ];
        let mut dd: Option<Driver> = None;
        let mut used = 0;
        for it in &items {
            let (Some(id), Some(label)) = (it["id"].as_str(), it["label"].as_str()) else { continue };
            if !label.ends_with('…') || it["enabled"] != json!(true) || skip.iter().any(|s| id.to_ascii_lowercase().contains(s)) {
                continue;
            }
            if used % 8 == 0 {
                let mut fresh = Driver::demo(size);
                fresh.call("ui.menu.invoke", json!({"id": lang()}));
                dd = Some(fresh);
            }
            used += 1;
            let Some(dd) = dd.as_mut() else { continue };
            select_first_clip(dd);
            if dd.call("ui.menu.invoke", json!({"id": id}))["ok"] == json!(true) {
                dd.inspect(&format!("{tag} dialog {id}"), &mut found);
            }
            for _ in 0..3 {
                dd.call("ui.key", json!({"key": "Escape"}));
            }
            dd.frames(2);
        }
    }
    if let Some(path) = report {
        let lines: Vec<String> = found.values().map(|f| json!({"kind": f.kind, "text": f.text, "screen": f.screen, "detail": f.detail}).to_string()).collect();
        std::fs::write(path, lines.join("\n")).unwrap();
    }
    let list: Vec<String> = found.values().map(|f| format!("{} [{}] “{}” ({})", f.kind, f.screen, f.text, f.detail)).collect();
    assert!(list.is_empty(), "{} translated labels do not fit:\n{}", list.len(), list.join("\n"));
}

/// Effect Controls, Lumetri Color, Properties and Essential Sound with a clip selected: every video
/// and audio effect's parameter labels, one effect at a time.
#[test]
fn russian_effect_and_clip_panels_fit() {
    let report = std::env::var_os("FILMCRAFT_FIT_REPORT").map(|p| std::path::PathBuf::from(format!("{}.clip", p.to_string_lossy())));
    let mut found = BTreeMap::new();
    let size = egui::vec2(1600.0, 980.0);
    let mut d = Driver::demo(size);
    assert_eq!(d.call("ui.menu.invoke", json!({"id": lang()}))["ok"], json!(true));
    let exec = |d: &mut Driver, c: &str, p: Value| d.call("engine.execute", json!({"command": c, "params": p}));
    let seq = exec(&mut d, "sequence.inspect", json!({}))["result"].clone();
    let video = seq["video"][0]["items"][0]["clip"].as_u64().unwrap();
    let audio = seq["audio"][0]["items"][0]["clip"].as_u64().unwrap();
    // video clip: Properties, Lumetri Color, Effect Controls per effect
    exec(&mut d, "timeline.select", json!({"clips": [video]}));
    exec(&mut d, "effects.apply", json!({"clips": [video], "effect": "lumetri"}));
    for panel in ["Properties", "EffectControls", "LumetriColor", "EssentialGraphics"] {
        d.call("ui.panel.show", json!({"panel": panel}));
        d.inspect(&format!("video clip {panel}"), &mut found);
    }
    for (kind, clip) in [("Video", video), ("Audio", audio)] {
        exec(&mut d, "timeline.select", json!({"clips": [clip]}));
        let list = exec(&mut d, "effects.list", json!({"kind": kind}))["result"].as_array().cloned().unwrap_or_default();
        for e in list {
            let Some(id) = e["id"].as_str() else { continue };
            if exec(&mut d, "effects.apply", json!({"clips": [clip], "effect": id}))["ok"] != json!(true) {
                continue;
            }
            d.call("ui.panel.show", json!({"panel": "EffectControls"}));
            d.inspect(&format!("1600x980 fx {id}"), &mut found);
            exec(&mut d, "edit.undo", json!({}));
        }
    }
    // every parameter label of every effect, measured: Effect Controls shows ~112 pt of label in
    // the default layout and sets a long label up to 20 % smaller before cutting it with "…"
    for kind in ["Video", "Audio"] {
        let list = exec(&mut d, "effects.list", json!({"kind": kind, "detail": true}))["result"].as_array().cloned().unwrap_or_default();
        for e in list {
            for p in e["paramInfo"].as_array().cloned().unwrap_or_default() {
                let Some(label) = p["label"].as_str() else { continue };
                let shown = d.harness.state().ui.language.tr(label).to_string();
                let w = d.harness.ctx.fonts_mut(|f| f.layout_no_wrap(shown.clone(), egui::FontId::proportional(12.0 * 0.8), egui::Color32::WHITE).size().x);
                if w > 112.0 {
                    found.entry((shown.clone(), "param too long")).or_insert(Finding {
                        kind: "param too long",
                        text: shown,
                        screen: format!("fx {}", e["id"].as_str().unwrap_or("")),
                        detail: format!("{w:.0} pt at 80 %"),
                    });
                }
            }
        }
    }
    // audio clip by type: Essential Sound and Properties
    for ty in ["dialogue", "music", "sfx", "ambience"] {
        exec(&mut d, "timeline.select", json!({"clips": [audio]}));
        exec(&mut d, "essentialSound.setType", json!({"clips": [audio], "type": ty}));
        for panel in ["EssentialSound", "Properties"] {
            d.call("ui.panel.show", json!({"panel": panel}));
            d.inspect(&format!("audio {ty} {panel}"), &mut found);
        }
    }
    if let Some(path) = report {
        let lines: Vec<String> = found.values().map(|f| json!({"kind": f.kind, "text": f.text, "screen": f.screen, "detail": f.detail}).to_string()).collect();
        std::fs::write(path, lines.join("\n")).unwrap();
    }
    let list: Vec<String> = found.values().map(|f| format!("{} [{}] “{}” ({})", f.kind, f.screen, f.text, f.detail)).collect();
    assert!(list.is_empty(), "{} translated labels do not fit:\n{}", list.len(), list.join("\n"));
}
