// The Remove background screen: drop photos, get transparent PNGs next to them.

use crate::jobs::{self, FileJobs, Work};
use crate::{cutout, theme};
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

pub struct CutoutView {
    jobs: FileJobs,
    picked: (Sender<Vec<PathBuf>>, Receiver<Vec<PathBuf>>),
}

impl CutoutView {
    pub fn new(ctx: &egui::Context) -> Self {
        CutoutView {
            jobs: FileJobs::new(ctx),
            picked: channel(),
        }
    }

    pub fn add(&mut self, paths: &[PathBuf]) {
        let work: Work = Arc::new(|file| {
            let out = cutout::remove_background(file)?;
            Ok((out, String::new()))
        });
        for file in jobs::collect(paths, cutout::supported) {
            self.jobs.add(file, work.clone());
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.jobs.poll();
        let (dropped, hovering) = theme::dropped_files(ui.ctx());
        if !dropped.is_empty() {
            self.add(&dropped);
        }
        while let Ok(files) = self.picked.1.try_recv() {
            self.add(&files);
        }

        theme::page_header(
            ui,
            "Remove background",
            "Saves a transparent PNG next to each photo as name-nobg. Runs on your computer, nothing is uploaded.",
        );
        ui.add_space(16.0);
        if theme::drop_zone(
            ui,
            hovering,
            "Drop photos here",
            "or click to choose · JPG, PNG, WebP · folders too · the first time it downloads the AI model (170 MB)",
        )
        .clicked()
        {
            jobs::pick_files(
                ui.ctx(),
                self.picked.0.clone(),
                "Remove background",
                ("Images", cutout::EXTS),
            );
        }
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height((ui.available_height() - 36.0).max(80.0))
            .show(ui, |ui| self.jobs.ui(ui));
        if !self.jobs.is_empty() && ui.button("Clear list").clicked() {
            self.jobs.clear();
        }
    }
}
