//! Labs › LAN dictation's drawer: the paired phones, each with Unpair, and
//! Pair a phone, which shows the pairing's QR code for Tiro for Android to
//! scan. The same pairing state `hex lan` works on (`crate::lan`).

use rune::{El, col, el, row, text};

use super::super::Ui;
use super::super::controls::{ButtonKind, DESCRIPTION, Paint, button, icon_button};
use crate::desktop::i18n::{tr, tr_fill};

/// A QR code's edge in points, quiet zone included: big enough for a phone's
/// camera from arm's length.
const QR_EDGE: f32 = 232.0;
/// Light modules around the code, as the QR standard asks.
const QUIET: usize = 4;

#[derive(Default)]
pub(in crate::rune_ui) struct Phones {
    names: Vec<String>,
    /// The pairing whose QR code shows, until Done.
    showing: Option<Showing>,
    error: Option<String>,
}

struct Showing {
    name: String,
    modules: Vec<Vec<bool>>,
}

/// Reads the paired phones again; the drawer calls it when it opens.
pub(super) fn refresh(ui: &mut Ui) {
    ui.lab_store.phones.names = crate::lan::device_names();
}

pub(super) fn body(ui: &Ui, p: Paint) -> El {
    let phones = &ui.lab_store.phones;
    let mut body = col().key("lan-phones").gap(10.0).child(
        text(tr("Phones"))
            .key("heading")
            .font(DESCRIPTION)
            .color(p.muted()),
    );
    if phones.names.is_empty() {
        body = body.child(
            text(tr("No phones paired yet"))
                .key("none")
                .font(p.k.ty.ui)
                .color(p.text()),
        );
    }
    for name in &phones.names {
        let unpair = name.clone();
        body = body.child(
            row()
                .key(format!("phone-{name}"))
                .gap(8.0)
                .center_y()
                .child(
                    text(name.as_str())
                        .key("name")
                        .font(p.k.ty.ui)
                        .color(p.text())
                        .grow(1.0),
                )
                .child(icon_button(
                    p,
                    "unpair",
                    &rune_theme_opencode::icons::TRASH,
                    &tr_fill("Unpair {}", name),
                    move |ui: &mut Ui| revoke(ui, &unpair),
                )),
        );
    }
    if let Some(showing) = &phones.showing {
        body = body
            .child(qr(&showing.modules))
            .child(
                text(tr("Scan with the phone's camera, then confirm in Tiro"))
                    .key("scan")
                    .font(DESCRIPTION)
                    .color(p.muted())
                    .wrap(),
            )
            .child(row().key("actions").child(button(
                p,
                "lan-pair-done",
                tr("Done"),
                ButtonKind::Secondary,
                |ui, _| {
                    ui.lab_store.phones.showing = None;
                    refresh(ui);
                },
            )));
    } else {
        body = body.child(row().key("actions").child(button(
            p,
            "lan-pair",
            tr("Pair a phone"),
            ButtonKind::Secondary,
            |ui, _| pair(ui),
        )));
    }
    if let Some(error) = &phones.error {
        body = body.child(
            text(error.as_str())
                .key("error")
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    }
    body
}

fn pair(ui: &mut Ui) {
    let phones = &mut ui.lab_store.phones;
    let made = crate::lan::pair(&crate::lan::unused_name("Phone"), None).and_then(|pairing| {
        Ok(Showing {
            modules: crate::lan::qr_modules(&pairing.link)?,
            name: pairing.name,
        })
    });
    match made {
        Ok(showing) => {
            phones.showing = Some(showing);
            phones.error = None;
        }
        Err(error) => phones.error = Some(tr_fill("Couldn't pair: {}", &error.to_string())),
    }
    refresh(ui);
}

fn revoke(ui: &mut Ui, name: &str) {
    let phones = &mut ui.lab_store.phones;
    phones.error = crate::lan::revoke(name)
        .err()
        .map(|error| tr_fill("Couldn't unpair: {}", &error.to_string()));
    if phones
        .showing
        .as_ref()
        .is_some_and(|showing| showing.name == name)
    {
        phones.showing = None;
    }
    refresh(ui);
}

/// The code as rows of dark runs on a light plate. Always dark on light,
/// whatever the theme: phone cameras read inverted codes unreliably.
fn qr(modules: &[Vec<bool>]) -> El {
    let span = modules.len() + 2 * QUIET;
    let module = (QR_EDGE / span as f32).floor().max(1.0);
    let mut code = col().key("qr-modules");
    for (y, line) in modules.iter().enumerate() {
        let mut cells = row().key(format!("r{y}")).h(module);
        let mut x = 0;
        while x < line.len() {
            let dark = line[x];
            let run = line[x..].iter().take_while(|cell| **cell == dark).count();
            let mut cell = el().w(run as f32 * module).h(module);
            if dark {
                cell = cell.fill(rune::Color::BLACK);
            }
            cells = cells.child(cell);
            x += run;
        }
        code = code.child(cells);
    }
    row().key("qr").child(
        el().key("plate")
            .role("img")
            .label(tr("Pairing QR code"))
            .padding(QUIET as f32 * module)
            .fill(rune::Color::WHITE)
            .child(code),
    )
}
