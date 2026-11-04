//! Android launcher wardrobe compatibility module.
//!
//! The original wardrobe implementation is missing from the repository snapshot used by
//! this workflow. This module restores the symbols required by the launcher so the Android
//! build can proceed. The skin-management UI remains explicitly unavailable until the full
//! implementation is restored.

use horizon_client::ui_theme::{Face, Ui};
use std::path::Path;
use winit::event::Touch;

#[derive(Clone, Copy)]
pub(super) enum CloudAction {
    Upload,
    Cape(usize),
}

#[derive(Default)]
pub(super) struct PreviewCamera {
    yaw: f32,
    pitch: f32,
    zoom: f32,
}

impl PreviewCamera {
    pub(super) fn rotate(&mut self, yaw: f32, pitch: f32) {
        self.yaw += yaw;
        self.pitch = (self.pitch + pitch).clamp(-80.0, 80.0);
    }

    pub(super) fn zoom(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.zoom = (self.zoom * factor).clamp(0.25, 4.0);
        }
    }
}

pub(super) struct Wardrobe {
    player_name: String,
    pub(super) capes: bool,
    pub(super) view: [f32; 3],
    pub(super) camera: PreviewCamera,
}

impl Wardrobe {
    pub(super) fn new(_home: &Path) -> Self {
        Self {
            player_name: String::new(),
            capes: false,
            view: [0.0; 3],
            camera: PreviewCamera::default(),
        }
    }

    pub(super) fn open(&mut self, player_name: &str, _client_id: &str) {
        self.player_name.clear();
        self.player_name.push_str(player_name);
        self.view = [0.0; 3];
    }

    pub(super) fn poll(&mut self) {}

    pub(super) fn reload(&mut self) {}

    pub(super) fn draw(
        &mut self,
        ui: &mut Ui,
        _targets: &mut Vec<([f32; 4], super::pages::Action)>,
        _scroll: f32,
    ) {
        self.view = [0.0, 0.0, 0.0];
        let width = (ui.w - 48.0).max(120.0);
        let x = (ui.w - width) * 0.5;
        let y = (ui.h - 128.0).max(160.0);

        ui.rounded(x, y, width, 128.0, 12.0, 0xff1d2028);
        ui.text(
            Face::Medium,
            18.0,
            x + 20.0,
            y + 34.0,
            0xfff1f0f8,
            "Skins & Capes",
        );
        ui.text(
            Face::Regular,
            14.0,
            x + 20.0,
            y + 66.0,
            0xff9699a8,
            "Wardrobe management is unavailable in this source snapshot.",
        );
        if !self.player_name.is_empty() {
            ui.text(
                Face::Regular,
                13.0,
                x + 20.0,
                y + 96.0,
                0xff9699a8,
                &format!("Player: {}", self.player_name),
            );
        }
    }

    pub(super) fn select(&mut self, _index: usize) {}

    pub(super) fn import(&mut self) {}

    pub(super) fn cloud(&mut self, _client_id: &str, _action: Option<CloudAction>) {}

    pub(super) fn use_cloud(&mut self) {}

    pub(super) fn model(&mut self) {}

    pub(super) fn preview(&mut self) {}

    pub(super) fn hide_cape(&mut self) {}

    pub(super) fn gesture(&mut self, _touch: &Touch, _density: f32) -> bool {
        false
    }

    pub(super) fn prepare_game(
        &self,
        _player_name: &str,
        _game_home: &Path,
    ) -> horizon_client::Result<()> {
        Ok(())
    }
}
