//! The live text plate (Labs): a click-through plate beside the recording
//! HUD with what the dictation has said so far, on every OS. Long text wraps
//! onto up to [`MAX_LINES`] lines, growing away from the HUD; past that the
//! oldest lines scroll out of view. It shows while a recording has text and hides when the
//! recording ends ([`crate::dictation::live_text`]); `rune_hud` says where the HUD is,
//! so the plate sits just past it, on the HUD's screen.
//!
//! The overlay is prewarmed: macOS keeps it drawn while hidden and refreshes
//! it before a reveal, so it never reappears with the last dictation's
//! words for a frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rune::{
    Anim, Cx, El, OverlayHandle, OverlayOptions, OverlayPlacement, OverlaySpec, ScreenEdge, Span,
    col, text,
};

use super::controls::Paint;
use crate::transcription::engine::Partial;

/// The overlay's size: about a dozen words a line, and room for the plate
/// at [`MAX_LINES`] (it hugs fewer).
pub(crate) const SIZE: (f32, f32) = (560.0, 120.0);
/// The plate's height with one line, and its corner radius's diameter.
const PLATE_H: f32 = 32.0;
/// The most lines the plate shows; older lines scroll out of view above.
const MAX_LINES: f32 = 4.0;
/// The overlay's room between its HUD-side edge and the plate.
const PAD: f32 = 6.0;

#[derive(Default)]
pub(crate) struct Plate {
    stable: String,
    tentative: String,
    /// The plate hangs down from the overlay's top (the HUD is above it);
    /// else it rises from the bottom.
    below: bool,
}

/// Where the plate starts, just past the HUD.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Spot {
    /// This far from the edge that the HUD is on, on the HUD's display
    /// (`rune_hud::display`).
    Edge(ScreenEdge, f32),
    /// At `y` in Rune's global space (points from the primary display's
    /// top-left, y down), centred on `x`; the plate hangs down from `y`
    /// when `below`, else rises from it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Global { x: f32, y: f32, below: bool },
}

impl Spot {
    /// Whether the plate sits below the HUD, so it grows downwards.
    fn below(self) -> bool {
        match self {
            Self::Edge(edge, _) => edge != ScreenEdge::Bottom,
            Self::Global { below, .. } => below,
        }
    }

    /// The overlay's placement for the plate to start at this spot.
    fn placement(self) -> OverlayPlacement {
        match self {
            Self::Edge(edge, margin) => OverlayPlacement::Edge {
                edge,
                margin: margin - PAD,
                align: rune::Align::Center,
                inset: 0.0,
            },
            Self::Global { x, y, below } => OverlayPlacement::Rect {
                x: x - SIZE.0 / 2.0,
                y: if below { y - PAD } else { y - SIZE.1 + PAD },
                width: SIZE.0,
                height: SIZE.1,
            },
        }
    }

    /// Places `overlay` for the plate to start at this spot.
    fn apply(self, overlay: &OverlayHandle<Plate>) {
        // Off macOS the HUD is placed with `place`, which picks the display
        // again and measures from the same edge of its work area (above the
        // Windows taskbar), so the plate is placed the same way and never
        // lands under the HUD (#797).
        #[cfg(not(target_os = "macos"))]
        if let Self::Edge(edge, margin) = self {
            overlay.place(edge, margin - PAD);
            return;
        }
        overlay.set_placement(Some(self.placement()));
    }
}

/// The plate's overlay, once installed.
static LIVE: std::sync::OnceLock<rune::OverlayHandle<Plate>> = std::sync::OnceLock::new();

/// Moves a shown plate to the display under the pointer when it changed
/// (`rune_hud`'s follow, #807).
#[cfg(not(target_os = "macos"))]
pub(crate) fn refollow_pointer() {
    if let Some(overlay) = LIVE.get() {
        overlay.refollow_pointer();
    }
}

/// Where the plate goes. Called on the app's thread each time the plate
/// appears, so it may read the screen.
pub(crate) type Place = fn() -> Spot;

/// Adds the plate to the app. It shows only while a dictation publishes its
/// text (Labs › Live text, applied at once). On macOS it's always
/// prewarmed, since Live text may be turned on after launch and an
/// unprewarmed plate reappears for a frame with the last dictation's words
/// (#1075); elsewhere only when Live text is on at launch.
pub(crate) fn install<S: 'static>(app: &mut rune::App<S>, place: Place) {
    let spec = OverlaySpec {
        name: "Hex live text",
        size: SIZE,
        // Placed for real each time it appears.
        edge: ScreenEdge::Top,
        margin: 0.0,
        align: rune::Align::Center,
        inset: 0.0,
        interactive: false,
        view,
    }
    .options(OverlayOptions {
        prewarm: cfg!(target_os = "macos") || crate::labs::live_text(),
        #[cfg(not(target_os = "macos"))]
        display: crate::rune_hud::display(),
        ..Default::default()
    })
    // Hidden, it holds no GPU surface (nor, on Windows, a window), so it
    // never passes for a game to capture tools (#1308).
    .release_gpu_when_hidden();
    let overlay = app.overlay(spec, Plate::default());
    let _ = LIVE.set(overlay.clone());
    let shown = Arc::new(AtomicBool::new(false));
    crate::dictation::live_text::watch(move |partial| {
        let partial = partial.filter(|partial| !is_blank(partial));
        let Some(Partial { stable, tentative }) = partial.cloned() else {
            if shown.swap(false, Ordering::AcqRel) {
                // Hidden from the same queue that shows it, so a recording
                // that ends as its plate appears never leaves it showing.
                // Its last words stay while it fades (macOS and Windows);
                // the next appearance replaces them first.
                let handle = overlay.clone();
                overlay.update(move |_, _| handle.fade_out(FADE));
            }
            return;
        };
        let appearing = !shown.swap(true, Ordering::AcqRel);
        let handle = overlay.clone();
        overlay.update(move |plate, tx| {
            plate.stable = stable;
            plate.tentative = tentative;
            tx.instant();
            if appearing {
                // Placed and shown after the new words, so it appears
                // where it belongs with them already drawn.
                let spot = place();
                plate.below = spot.below();
                spot.apply(&handle);
                handle.show();
            }
        });
    });
}

/// How long the plate fades out once a dictation stops publishing.
const FADE: Duration = Duration::from_millis(150);

/// The model's first update often has no words yet.
fn is_blank(partial: &Partial) -> bool {
    partial.stable.trim().is_empty() && partial.tentative.trim().is_empty()
}

/// The words as one string, and where the tentative ones start. Stable and
/// tentative words share one paragraph so they wrap together; its bidi
/// ordering puts right-to-left tentative words after (left of) the stable ones.
fn words(plate: &Plate) -> (String, usize) {
    let stable = plate.stable.trim();
    let tentative = if stable.is_empty() {
        plate.tentative.trim_start()
    } else {
        plate.tentative.as_str()
    };
    (format!("{stable}{}", tentative.trim_end()), stable.len())
}

fn view(plate: &Plate, cx: &mut Cx<Plate>) -> El {
    let p = Paint::new(cx.system_dark(), cx.env().scale);
    let c = &p.k.c;
    let root = col()
        .key("live-text")
        .w_full()
        .h_full()
        .items_center()
        .px(4.0)
        .py(PAD)
        .hover_motion_within(Anim::INSTANT, Anim::INSTANT);
    // Grows away from the HUD.
    let root = if plate.below {
        root
    } else {
        root.justify_end()
    };
    let (words, tentative_at) = words(plate);
    if words.trim().is_empty() {
        return root;
    }
    let font = p.k.ty.ui;
    let line_h = if font.line_height > 0.0 {
        font.line_height
    } else {
        font.size * 1.25
    };
    let pad_y = ((PLATE_H - line_h) / 2.0).max(0.0);
    let tentative = Span::new(tentative_at, words.len()).color(p.faint());
    let words = text(words)
        .key("words")
        .wrap()
        .spans(vec![tentative])
        .font(font)
        .color(p.text());
    root.child(
        col()
            .key("plate")
            .min_h(PLATE_H)
            .max_w(SIZE.0 - 8.0)
            .px(14.0)
            .py(pad_y)
            .justify_end()
            .radius(PLATE_H / 2.0)
            .fill(c.bg_base)
            .border(p.hair, c.border_base)
            // The newest words stay in view: older lines leave through
            // the top, clipped at a line's edge.
            .child(
                col()
                    .key("line")
                    .max_h(line_h * MAX_LINES)
                    .justify_end()
                    .clip_rounded(0.0)
                    .child(words),
            ),
    )
}

#[cfg(test)]
mod tests {
    use rune::Runtime;
    use rune::text::TextSystem;

    use super::*;

    const SHORT: &str = "So the plan for today";
    const LONG: &str = "So the plan for today is to finish the release notes, then check the \
        streaming numbers on the M4, and after lunch we can review the onboarding flow together";
    const HEBREW: &str = "אז התוכנית להיום";

    fn runtime(stable: &str, tentative: &str, dark: bool) -> Runtime<Plate> {
        runtime_at(stable, tentative, dark, true)
    }

    fn runtime_at(stable: &str, tentative: &str, dark: bool, below: bool) -> Runtime<Plate> {
        let plate = Plate {
            stable: stable.into(),
            tentative: tentative.into(),
            below,
        };
        let mut rt = crate::rune_ui::offscreen::runtime(
            plate,
            view,
            SIZE,
            rune::Env {
                system_dark: dark,
                scale: 2.0,
            },
            TextSystem::new(rune_theme_opencode::FONTS, rune_theme_opencode::FAMILIES),
        );
        rt.settle_all();
        rt.frame(0.0);
        rt
    }

    fn rect(rt: &Runtime<Plate>, key: &str) -> Option<[f32; 4]> {
        rt.elements()
            .into_iter()
            .find(|e| !e.exiting && e.key.as_deref() == Some(key))
            .map(|e| e.rect)
    }

    #[test]
    fn a_short_line_hugs_its_words_on_the_huds_side() {
        for below in [true, false] {
            let rt = runtime_at(SHORT, "", false, below);
            let [x, y, w, h] = rect(&rt, "plate").expect("the plate draws");
            assert!(w < SIZE.0 / 2.0, "the plate hugs a short line: {w}");
            assert!((x + w / 2.0 - SIZE.0 / 2.0).abs() < 1.0, "centred: {x} {w}");
            assert_eq!(h, PLATE_H);
            if below {
                assert!((y - PAD).abs() < 0.5, "hangs from the top: {y}");
            } else {
                assert!(
                    (y + h - (SIZE.1 - PAD)).abs() < 0.5,
                    "rises from the bottom: {y} {h}"
                );
            }
        }
    }

    #[test]
    fn a_long_line_wraps_inside_the_overlay() {
        for tentative in ["", " together with the team"] {
            let rt = runtime(LONG, tentative, true);
            let [x, y, w, h] = rect(&rt, "plate").unwrap();
            assert!(x >= 0.0 && x + w <= SIZE.0, "inside: {x} {w}");
            assert!(y >= 0.0 && y + h <= SIZE.1, "inside: {y} {h}");
            assert!(h > PLATE_H, "more than one line: {h}");
            let [wx, wy, ww, wh] = rect(&rt, "words").unwrap();
            assert!(
                wx >= x && wx + ww <= x + w,
                "the words fit the plate: {wx} {ww}"
            );
            assert!(wy >= y && wy + wh <= y + h, "every line shows: {wy} {wh}");
        }
    }

    #[test]
    fn past_the_last_line_the_oldest_lines_scroll_away() {
        let long = [LONG; 4].join(" ");
        let rt = runtime(&long, " and the newest words", false);
        let [_, y, _, h] = rect(&rt, "plate").unwrap();
        assert!(
            y >= 0.0 && y + h <= SIZE.1,
            "the plate stays inside: {y} {h}"
        );
        let [_, ly, _, lh] = rect(&rt, "line").unwrap();
        let [_, wy, _, wh] = rect(&rt, "words").unwrap();
        assert!(wy < ly, "older lines leave through the top: {wy} {ly}");
        assert!(
            (wy + wh - (ly + lh)).abs() < 0.5,
            "the newest line sits at the bottom: {wy} {wh} {ly} {lh}"
        );
    }

    #[test]
    fn stable_and_tentative_words_share_one_paragraph() {
        assert_eq!(
            words(&Plate {
                stable: " So the plan ".into(),
                tentative: " is to finish ".into(),
                below: true,
            }),
            ("So the plan is to finish".into(), 11)
        );
        assert_eq!(
            words(&Plate {
                stable: "  ".into(),
                tentative: " so".into(),
                below: true,
            }),
            ("so".into(), 0)
        );
        let (hebrew, at) = words(&Plate {
            stable: HEBREW.into(),
            tentative: " והיום".into(),
            below: true,
        });
        assert_eq!(&hebrew[at..], " והיום");
    }

    #[test]
    fn a_spot_places_the_plate_just_past_the_hud() {
        assert_eq!(
            Spot::Edge(ScreenEdge::Bottom, 50.0).placement(),
            OverlayPlacement::Edge {
                edge: ScreenEdge::Bottom,
                margin: 50.0 - PAD,
                align: rune::Align::Center,
                inset: 0.0,
            }
        );
        let rect = |below| match (Spot::Global {
            x: 700.0,
            y: 100.0,
            below,
        })
        .placement()
        {
            OverlayPlacement::Rect { x, y, width, .. } => (x, y, width),
            other => panic!("{other:?}"),
        };
        // Centred on the HUD; the plate's top edge at the spot when hanging
        // below it, its bottom edge at the spot when rising above it.
        assert_eq!(rect(true), (700.0 - SIZE.0 / 2.0, 100.0 - PAD, SIZE.0));
        assert_eq!(
            rect(false),
            (700.0 - SIZE.0 / 2.0, 100.0 - SIZE.1 + PAD, SIZE.0)
        );
        assert!(Spot::Edge(ScreenEdge::Top, 40.0).below());
        assert!(!Spot::Edge(ScreenEdge::Bottom, 40.0).below());
    }

    #[test]
    fn no_words_draw_nothing() {
        let rt = runtime("  ", "", false);
        assert!(rect(&rt, "plate").is_none());
        let blank = |stable: &str, tentative: &str| {
            is_blank(&Partial {
                stable: stable.into(),
                tentative: tentative.into(),
            })
        };
        assert!(blank("", ""));
        assert!(blank("  ", " "));
        assert!(!blank("", " so"));
        assert!(!blank("so", ""));
    }

    /// Renders the plate to `HEX_LIVE_TEXT_SNAPS/live-text-*.png` in both
    /// appearances, to look at.
    #[test]
    #[ignore = "set HEX_LIVE_TEXT_SNAPS to a folder; needs a GPU"]
    fn snapshots() {
        let folder = std::path::PathBuf::from(std::env::var("HEX_LIVE_TEXT_SNAPS").unwrap());
        let gpu = rune::render::Gpu::try_new(rune::render::Gpu::instance(), None).unwrap();
        let overflow = [LONG; 4].join(" ");
        for (name, stable, tentative) in [
            ("short", SHORT, ""),
            ("long", LONG, ""),
            ("overflow", overflow.as_str(), " and the newest words"),
            ("tentative", SHORT, " is to finish"),
            ("rtl", HEBREW, " והיום"),
        ] {
            for dark in [false, true] {
                let mut rt = runtime(stable, tentative, dark);
                let (w, h) = ((SIZE.0 * 2.0) as u32, (SIZE.1 * 2.0) as u32);
                let mut renderer = crate::rune_ui::offscreen::renderer(&gpu.device);
                let target = crate::rune_ui::offscreen::target(&gpu.device, w, h);
                renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
                renderer.prepare(
                    &gpu.device,
                    &gpu.queue,
                    &mut rt.text.atlas,
                    &rt.instances,
                    (w, h),
                );
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                // A desktop grey behind the clear overlay.
                let backdrop = if dark { 0.08 } else { 0.62 };
                renderer.draw(
                    &mut encoder,
                    &target.view,
                    [backdrop, backdrop, backdrop, 1.0],
                );
                target.copy(&mut encoder);
                gpu.queue.submit([encoder.finish()]);
                let theme = if dark { "dark" } else { "light" };
                let path = folder.join(format!("live-text-{name}-{theme}.png"));
                rune::render::write_png(&path, w, h, &target.read(&gpu.device)).unwrap();
            }
        }
    }
}
