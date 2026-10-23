// Support: toolswitch is free; a coffee by Cafecito (pesos) or USDT helps keep it going.

use crate::{Page, theme};
use eframe::egui;

pub const CAFECITO: &str = "https://cafecito.app/berchj";
pub const USDT: &str = "0xc39dfD297BAf6ab305e267D153bf11d37de7faF6";

#[derive(Default)]
pub struct SupportPage {
    copied: bool,
}

impl Page for SupportPage {
    fn opened(&mut self) {
        self.copied = false;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        theme::page_header(
            ui,
            "Support toolswitch",
            "toolswitch is free and made by one person. If it saves you time, you can buy me a coffee.",
        );
        ui.add_space(16.0);

        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("☕ Cafecito").strong());
            ui.label(egui::RichText::new("From Argentina, in pesos, with Mercado Pago or a card.").color(theme::MUTED));
            ui.add_space(8.0);
            let button = egui::Button::new(egui::RichText::new("Open cafecito.app/berchj").color(egui::Color32::WHITE))
                .fill(theme::ACCENT);
            if ui.add(button).clicked() {
                theme::open_folder(std::path::Path::new(CAFECITO));
            }
        });
        ui.add_space(10.0);

        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("💵 USDT").strong());
            ui.label(egui::RichText::new("From anywhere in the world.").color(theme::MUTED));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(USDT).monospace().size(12.0));
                let label = if self.copied { "✔ Copied" } else { "Copy" };
                if ui.button(label).clicked() {
                    ui.ctx().copy_text(USDT.into());
                    self.copied = true;
                }
            });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "⚠ Send it only on the BNB Smart Chain (BEP20) network. On any other network (Ethereum, Tron, Polygon…) it's lost.",
                )
                .size(12.0)
                .color(egui::Color32::from_rgb(0xf5, 0x9e, 0x0b)),
            );
        });
        ui.add_space(16.0);
        ui.label(egui::RichText::new("Thank you!").color(theme::MUTED));
    }
}
