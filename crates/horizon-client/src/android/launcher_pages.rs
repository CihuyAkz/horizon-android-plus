use super::*;
use horizon_client::{osk::Output, ui_theme::color};
use winit::{
    event::{KeyEvent, Touch},
    keyboard::{KeyCode, PhysicalKey},
};

fn animation_phase() -> f32 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}
fn panel(ui: &mut Ui, rect: [f32; 4], radius: f32, strong: bool) {
    let [x, y, w, h] = rect;
    ui.rounded(x, y, w, h, radius, if strong { color::raised() } else { color::surface() });
    ui.border(x, y, w, h, radius, color::border());
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Action {
    Page(Page),
    Play,
    Search,
    Field(&'static str),
    FinishNameEdit,
    Install(usize),
    Import(horizon_client::content_files::Kind),
    OpenFolder(horizon_client::content_files::Kind),
    ToggleSection(usize),
    ToggleLocal(usize),
    DeleteLocal(usize),
    ConfirmDeleteLocal,
    Next,
    Previous,
    Profile,
    Logs,
    RecordPerformance,
    ExportPerformance,
    Account,
    SignIn,
    DeviceSignIn,
    CopyLoginCode,
    CancelLogin,
    ReviewApp,
    SignOut,
    LoginLink,
    CloseLogin,
    Renderer,
    RenderScale,
    ChooseRenderScale(crate::display::RenderScale),
    Orientation,
    InputHost,
    ChooseInputHost(crate::catalog::InputHost),
    ChooseOrientation(crate::catalog::Orientation),
    ChooseRenderer(crate::catalog::RendererMode),
    ChooseProfile(u32),
    Dismiss,
    Immersive,
    Cutout,
    Aspect,
    ChooseAspect(crate::display::Aspect),
    DisplayHz,
    ChooseDisplayHz(u16),
    PerformanceHints,
    GpuProfile,
    ToggleTheme,
    ToggleDrawer,
    CloseDrawer,
}

pub(super) fn button(
    ui: &mut Ui,
    targets: &mut Vec<([f32; 4], Action)>,
    rect: [f32; 4],
    label: &str,
    action: Action,
    accent: bool,
) {
    let [x, y, w, h] = rect;
    let radius = 10.0;
    let fill = if accent { color::accent() } else { color::raised() };
    ui.rounded(x, y, w, h, radius, fill);
    ui.border(x, y, w, h, radius, color::border());
    let padding = 14.0f32.min((w * 0.10).max(6.0));
    let base = 11.0;
    let font = if matches!(action, Action::Page(_)) {
        (base * ((w - 2.0 * padding) / ui.measure(Face::Medium, base, label).max(1.0)).min(1.0)).max(9.0)
    } else {
        base.min((h / 1.8).max(10.0))
    };
    let label = ui.fit_text(Face::Medium, font, label, w - 2.0 * padding);
    let text_y = y + ((h - font * 1.45) * 0.5).max(0.0);
    ui.text(Face::Medium, font, x + padding, text_y, if accent { color::on_accent() } else { color::text() }, &label);
    targets.push((ui.screen_rect(rect), action));
}

fn toggle(
    ui: &mut Ui,
    targets: &mut Vec<([f32; 4], Action)>,
    rect: [f32; 4],
    on: bool,
    action: Action,
) {
    let [x, y, w, h] = rect;
    let track_x = x + w - 37.0;
    let track_y = y + (h - 21.0) * 0.5;
    ui.rounded(track_x, track_y, 37.0, 21.0, 10.5, if on { color::accent() } else { color::surface_3() });
    ui.rounded(track_x + if on { 19.0 } else { 3.0 }, track_y + 3.0, 15.0, 15.0, 7.5, if on { color::on_accent() } else { color::muted() });
    targets.push((ui.screen_rect(rect), action));
}

fn nav_button(
    ui: &mut Ui,
    targets: &mut Vec<([f32; 4], Action)>,
    rect: [f32; 4],
    label: &str,
    icon: &str,
    action: Action,
    active: bool,
) {
    let [x, y, w, h] = rect;
    if active {
        ui.rounded(x, y, w, h, 11.0, color::accent_soft());
    }
    let narrow = w < 180.0;
    let icon_x = x + if narrow { 9.0 } else { 12.0 };
    let label_x = x + if narrow { 35.0 } else { 41.0 };
    let font = if narrow { 11.0 } else { 12.5 };
    ui.sprite_tinted(
        &format!("horizon:gui/icons/{icon}"),
        icon_x,
        y + (h - 17.0) * 0.5,
        17.0,
        17.0,
        if active { color::accent() } else { color::muted() },
    );
    let label = ui.fit_text(Face::Bold, font, label, (w - label_x + x - 8.0).max(20.0));
    ui.text(
        Face::Bold,
        font,
        label_x,
        y + (h - 18.0) * 0.5,
        if active { color::accent() } else { color::muted() },
        &label,
    );
    targets.push((ui.screen_rect(rect), action));
}

impl Launcher {
    /// The same spatial focus algorithm as the game's controller menus.
    pub(super) fn navigate(&mut self, key: &KeyEvent) -> bool {
        self.navigate_key(&key.logical_key)
    }
    pub(super) fn navigate_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use horizon_client::gamepad::{self, Button};
        use winit::keyboard::{Key, NamedKey};
        if self.pending_focus.is_some() {
            return true;
        }
        if self.catalog.editing.is_some() || self.targets.is_empty() {
            return false;
        }
        let button = match key {
            Key::Named(NamedKey::ArrowUp) => Button::DPadUp,
            Key::Named(NamedKey::ArrowDown) => Button::DPadDown,
            Key::Named(NamedKey::ArrowLeft) => Button::DPadLeft,
            Key::Named(NamedKey::ArrowRight) => Button::DPadRight,
            Key::Named(NamedKey::Tab) => {
                self.focus = Some(self.focus.map_or(0, |i| (i + 1) % self.targets.len()));
                return true;
            }
            _ => return false,
        };
        if matches!(button, Button::DPadUp | Button::DPadDown)
            && let Some((_, action)) = self.focus.and_then(|i| self.targets.get(i))
            && let Some(index) = self.list_targets.iter().position(|(_, a)| a == action)
        {
            let rects: Vec<_> = self.list_targets.iter().map(|(r, _)| *r).collect();
            if let Some(next) = gamepad::focus_step_strict(rects[index], button, &rects)
                && let Some(view) = self.list_view
            {
                let (rect, action) = self.list_targets[next];
                let scroll = if let Some(chooser) = &mut self.chooser {
                    &mut chooser.scroll
                } else {
                    &mut self.catalog.scroll
                };
                *scroll = super::gesture::reveal(*scroll, rect, view);
                self.pending_focus = Some(action);
                self.focus = None;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                return true;
            }
        }
        self.focus = Some(
            if let Some(index) = self.focus.filter(|i| *i < self.targets.len()) {
                let rects: Vec<_> = self.targets.iter().map(|(rect, _)| *rect).collect();
                gamepad::focus_step_rect(rects[index], button, &rects).unwrap_or(index)
            } else {
                0
            },
        );
        true
    }

    pub(super) fn draw_pages(&mut self) -> Result<()> {
        let (Some(r), Some(gui)) = (&mut self.renderer, &self.gui) else {
            return Ok(());
        };
        if r.suspended {
            return Ok(());
        }
        self.targets.clear();
        self.list_targets.clear();
        self.list_view = None;
        self.sidebar_view = None;
        let mut b = HudBatch::new([r.config.width, r.config.height]);
        let mut ui = Ui::mobile(&gui.atlas, &mut b, (0.0, 0.0));
        // OEM navigation bars can remain visible even in fullscreen (keypad phones).
        // Inset the launcher layout only; keep surface and input coordinates intact.
        if let Some(window) = &self.window {
            use winit::platform::android::WindowExtAndroid;
            let rect = window.content_rect();
            let input = winit::platform::android::native_input_scale();
            if rect.bottom > 0 {
                ui.h = ui.h.min(rect.bottom as f32 * input[1] as f32 / ui.density);
            }
            if rect.right > 0 {
                ui.w = ui.w.min(rect.right as f32 * input[0] as f32 / ui.density);
            }
        }
        if let Some(bottom) = crate::native_ime::viewport_bottom() {
            ui.h = ui.h.min(bottom as f32 / ui.density);
        }
        let (screen_w, screen_h) = (ui.w, ui.h);
        let animation = animation_phase();
        let scale = ui.density / ui.batch.scale;
        ui.batch.fill(&gui.atlas, 0.0, 0.0, screen_w * scale, screen_h * scale, color::bg());

        let landscape_short = screen_w > screen_h && screen_h <= 600.0;
        let landscape_narrow = landscape_short && screen_w <= 900.0;
        let drawer_mode = screen_w <= 900.0 || screen_h <= 560.0;
        if !drawer_mode {
            self.drawer_open = false;
        }
        let sidebar_width = if drawer_mode {
            if landscape_narrow { 156.0 } else { (screen_w * 0.82).min(290.0) }
        } else if landscape_short {
            205.0
        } else if screen_w <= 1100.0 {
            225.0
        } else {
            252.0
        };
        let topbar_height = if landscape_short {
            54.0
        } else if drawer_mode {
            62.0
        } else {
            72.0
        };
        let permanent_sidebar = !drawer_mode;
        self.sidebar_width = sidebar_width;
        let main_x = if permanent_sidebar { sidebar_width } else { 0.0 };
        if drawer_mode && self.drawer_open {
            ui.rounded(sidebar_width, 0.0, (screen_w - sidebar_width).max(0.0), screen_h, 0.0, horizon_client::hud::rgba(0, 0, 0, 136));
            self.targets.push((ui.screen_rect([sidebar_width, 0.0, (screen_w - sidebar_width).max(0.0), screen_h]), Action::CloseDrawer));
        }
        if permanent_sidebar || self.drawer_open {
            ui.rounded(0.0, 0.0, sidebar_width, screen_h, 0.0, color::surface());
            ui.rounded(sidebar_width - 1.0, 0.0, 1.0, screen_h, 0.0, color::border());
            let side_pad = if landscape_narrow { 10.0 } else { 15.0 };
            let brand_inner_pad = if landscape_narrow { 4.0 } else { 6.0 };
            let brand_y = if landscape_narrow { 20.0 } else if landscape_short && drawer_mode { 16.0 } else if drawer_mode { 26.0 } else { 31.0 };
            let brand_bottom = if landscape_narrow { 18.0 } else if landscape_short && drawer_mode { 12.0 } else { 24.0 };
            let mut brand_w = sidebar_width - side_pad * 2.0 - brand_inner_pad * 2.0;
            if screen_w <= 760.0 && !landscape_short {
                brand_w = brand_w.min(180.0);
            }
            let brand_h = (brand_w * 160.0 / 1024.0).min(if landscape_narrow { 54.0 } else if screen_w <= 760.0 && !landscape_short { 64.0 } else { 76.0 });
            ui.sprite("horizon:gui/launcher-logo", side_pad + brand_inner_pad, brand_y, brand_w, brand_h);
            let compact_sidebar = landscape_short && screen_h <= 560.0;
            let nav_height = if compact_sidebar { 38.0 } else { 43.0 };
            let nav_gap = 4.0;
            let section_height = if compact_sidebar { 28.0 } else { 35.0 };
            let section_text_top = if compact_sidebar { 8.0 } else { 12.0 };
            let nav_x = side_pad;
            let nav_w = sidebar_width - side_pad * 2.0;
            let nav_start = brand_y + brand_h + brand_bottom;
            let nav_total = 4.0 * section_height + 6.0 * nav_height + 2.0 * nav_gap;
            let footer_y = screen_h - 64.0;
            let nav_bottom = (footer_y - 8.0).max(nav_start);
            let nav_height_visible = (nav_bottom - nav_start).max(1.0);
            let nav_maximum = (nav_total - nav_height_visible).max(0.0);
            self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, nav_maximum);
            self.sidebar_view = Some([nav_start, nav_bottom, nav_maximum]);
            let mut nav_y = nav_start - self.sidebar_scroll;
            let section_size = 10.0;
            let groups = [
                ("LAUNCHER", vec![(Page::Play, "Home", "home")]),
                ("DOWNLOADS", vec![(Page::Mods, "Mods", "package"), (Page::Packs, "Resource Packs", "box"), (Page::Shaders, "Shader Packs", "wand")]),
                ("LIBRARY", vec![(Page::Installed, "Installed", "library")]),
                ("PREFERENCES", vec![(Page::Settings, "Settings", "settings")]),
            ];
            for (group, entries) in groups {
                if nav_y >= nav_start && nav_y + section_height <= nav_bottom {
                    ui.text(Face::Bold, section_size, nav_x + 11.0, nav_y + section_text_top, color::muted_2(), group);
                }
                nav_y += section_height;
                let entry_count = entries.len();
                for (entry_index, (page, label, icon)) in entries.into_iter().enumerate() {
                    if nav_y >= nav_start && nav_y + nav_height <= nav_bottom {
                        nav_button(
                            &mut ui,
                            &mut self.targets,
                            [nav_x, nav_y, nav_w, nav_height],
                            label,
                            icon,
                            Action::Page(page),
                            self.catalog.page == page,
                        );
                    }
                    nav_y += nav_height;
                    if entry_index + 1 < entry_count {
                        nav_y += nav_gap;
                    }
                }
            }
            if nav_maximum > 0.0 {
                let thumb_h = (nav_height_visible * nav_height_visible / (nav_height_visible + nav_maximum)).max(18.0).min(nav_height_visible);
                let thumb_y = nav_start + (nav_height_visible - thumb_h) * (self.sidebar_scroll / nav_maximum);
                ui.rounded(sidebar_width - 5.0, nav_start, 2.0, nav_height_visible, 1.0, color::border());
                ui.rounded(sidebar_width - 5.0, thumb_y, 2.0, thumb_h, 1.0, color::muted());
            }
            ui.rounded(nav_x + 4.0, footer_y, nav_w - 8.0, 1.0, 0.0, color::border());
            ui.rounded(nav_x + 4.0, footer_y + 15.0, 34.0, 34.0, 11.0, color::hex(0xe6c58b, 255));
            let initial = self.catalog.config.name.chars().next().unwrap_or('P').to_ascii_uppercase().to_string();
            ui.text_centered(Face::Medium, 14.0, nav_x + 21.0, footer_y + 22.0, color::hex(0x292017, 255), &initial);
            if sidebar_width >= 205.0 || drawer_mode && !landscape_narrow {
                ui.text(Face::Medium, 11.0, nav_x + 46.0, footer_y + 15.0, color::text(), &self.catalog.config.name);
                ui.text(Face::Regular, 10.0, nav_x + 46.0, footer_y + 31.0, color::muted(), "Player profile");
                ui.sprite_tinted("horizon:gui/icons/more", sidebar_width - 43.0, footer_y + 24.0, 17.0, 17.0, color::muted());
            }
            self.targets.push((ui.screen_rect([nav_x, footer_y + 10.0, nav_w, 43.0]), Action::Account));
        }
        ui.set_origin(main_x, 0.0);
        ui.w = (screen_w - main_x).max(1.0);
        let (main_w, _) = (ui.w, ui.h);
        ui.rounded(0.0, 0.0, main_w, topbar_height, 0.0, color::bg());
        ui.rounded(0.0, topbar_height - 1.0, main_w, 1.0, 0.0, color::border());
        let menu_x = if drawer_mode { 12.0 } else { 35.0 };
        if drawer_mode {
            ui.rounded(menu_x, (topbar_height - 36.0) * 0.5, 36.0, 36.0, 11.0, color::surface());
            ui.border(menu_x, (topbar_height - 36.0) * 0.5, 36.0, 36.0, 11.0, color::border());
            ui.sprite_tinted("horizon:gui/icons/layers", menu_x + 9.0, (topbar_height - 36.0) * 0.5 + 9.0, 18.0, 18.0, color::muted());
            self.targets.push((ui.screen_rect([menu_x, (topbar_height - 36.0) * 0.5, 36.0, 36.0]), Action::ToggleDrawer));
        }
        let crumb_size = if main_w <= 480.0 { 10.0 } else { 12.0 };
        let crumb_x = if drawer_mode { 60.0 } else { menu_x };
        ui.text(Face::Medium, crumb_size, crumb_x, (topbar_height - crumb_size * 1.45) * 0.5, color::text(), "Horizon Android");
        if main_w >= 350.0 {
            let right_pad = if drawer_mode { 12.0 } else if landscape_short { 24.0 } else if main_w <= 1100.0 { 25.0 } else { 35.0 };
            let search_w = if main_w <= 760.0 { 38.0 } else { 220.0 };
            let control_gap = if main_w <= 760.0 { 6.0 } else { 9.0 };
            let controls_w = search_w + 54.0 + 36.0 + 36.0 + control_gap * 3.0;
            let search_x = main_w - right_pad - controls_w;
            let search_y = (topbar_height - 36.0) * 0.5;
            ui.rounded(search_x, search_y, search_w, 36.0, 10.0, if search_w > 40.0 { color::surface() } else { color::bg() });
            if search_w > 40.0 {
                ui.border(search_x, search_y, search_w, 36.0, 10.0, color::border());
                ui.sprite_tinted("horizon:gui/icons/search", search_x + 10.0, search_y + 10.0, 15.0, 15.0, color::muted());
                ui.text(Face::Regular, 11.0, search_x + 33.0, search_y + 10.0, color::muted(), "Search launcher...");
                ui.text(Face::Regular, 9.0, search_x + search_w - 48.0, search_y + 12.0, color::muted_2(), "CTRL K");
            } else {
                ui.sprite_tinted("horizon:gui/icons/search", search_x + 10.0, search_y + 10.0, 18.0, 18.0, color::muted());
            }
            self.targets.push((ui.screen_rect([search_x, search_y, search_w, 36.0]), Action::Field("search")));
            let lang_x = search_x + search_w + control_gap;
            ui.rounded(lang_x, search_y, 54.0, 36.0, 10.0, color::surface());
            ui.border(lang_x, search_y, 54.0, 36.0, 10.0, color::border());
            ui.text(Face::Medium, 11.0, lang_x + 13.0, search_y + 10.0, color::text(), "ID");
            let bell_x = lang_x + 54.0 + control_gap;
            ui.rounded(bell_x, search_y, 36.0, 36.0, 11.0, color::surface());
            ui.border(bell_x, search_y, 36.0, 36.0, 11.0, color::border());
            ui.sprite_tinted("horizon:gui/icons/bell", bell_x + 10.0, search_y + 10.0, 16.0, 16.0, color::muted());
            let theme_x = bell_x + 36.0 + control_gap;
            ui.rounded(theme_x, search_y, 36.0, 36.0, 11.0, color::surface());
            ui.border(theme_x, search_y, 36.0, 36.0, 11.0, color::border());
            let theme_icon = if color::is_light() { "moon" } else { "sun" };
            ui.sprite_tinted(&format!("horizon:gui/icons/{theme_icon}"), theme_x + 10.0, search_y + 10.0, 16.0, 16.0, color::muted());
            self.targets.push((ui.screen_rect([theme_x, search_y, 36.0, 36.0]), Action::ToggleTheme));
        }
        let title = match self.catalog.page {
            Page::Play => "Home",
            Page::Mods | Page::Packs | Page::Shaders => "Downloads",
            Page::Installed => "Library",
            Page::Settings => "Settings",
        };
        let subtitle = match self.catalog.page {
            Page::Play => "Your game and profile",
            Page::Mods => "Browse Horizon mods",
            Page::Packs => "Browse resource packs on Modrinth",
            Page::Shaders => "Browse shader packs on Modrinth",
            Page::Installed => "Manage files installed in this profile",
            Page::Settings => "Configure rendering and device preferences",
        };
        let page = self.catalog.page;
        let padding_x = if drawer_mode { if landscape_narrow { 16.0 } else { 15.0 } } else if landscape_short { 24.0 } else if main_w <= 1100.0 { 25.0 } else { 35.0 };
        let padding_top = if drawer_mode { if landscape_narrow { 14.0 } else { 24.0 } } else if landscape_short { 18.0 } else if main_w <= 1100.0 { 28.0 } else { 34.0 };
        let padding_bottom = if drawer_mode { if landscape_narrow { 20.0 } else { 40.0 } } else if landscape_short { 22.0 } else if main_w <= 1100.0 { 45.0 } else { 54.0 };
        let content_w = main_w.min(1450.0);
        let content_x = (main_w - content_w) * 0.5 + padding_x;
        let content_y = topbar_height + padding_top;
        let inner_w = (content_w - 2.0 * padding_x).max(1.0);
        ui.set_origin(main_x + content_x, content_y);
        ui.w = inner_w;
        ui.h = (screen_h - content_y - padding_bottom).max(1.0);
        let (w, h) = (ui.w, ui.h);
        let compact = w <= 760.0;
        if page != Page::Play {
            let eyebrow = match page {
                Page::Mods | Page::Packs | Page::Shaders => "DOWNLOADS",
                Page::Installed => "LIBRARY",
                Page::Settings => "PREFERENCES",
                Page::Play => "LAUNCHER",
            };
            ui.text(Face::Medium, 10.0, 0.0, 0.0, color::accent(), eyebrow);
            ui.text(Face::Bold, if compact { 24.0 } else { 27.0 }, 0.0, 17.0, color::text(), title);
            ui.text(Face::Regular, 12.0, 0.0, 54.0, color::muted(), subtitle);
        }
        if page == Page::Play {
            let hero_height = if landscape_short {
                (screen_h - 75.0).max(190.0)
            } else if compact {
                (screen_h - 130.0).max(300.0)
            } else {
                (screen_h - 145.0).max(320.0)
            };
            let hero_y = 0.0;
            let hero_w = w;
            let glow_w = (hero_w * 0.84).min(820.0);
            let glow_h = glow_w * 512.0 / 819.0;
            ui.sprite("horizon:gui/launcher-home-glow", (hero_w - glow_w) * 0.5, (hero_height - glow_h) * 0.5 - 12.0, glow_w, glow_h);
            let center_x = hero_w * 0.5;
            let button_w = if compact { 210.0 } else { 230.0 };
            let button_h = if compact { 78.0 } else { 86.0 };
            let ready_y = hero_y + (hero_height - button_h - 22.0 - 16.0) * 0.5;
            let ready = "READY TO PLAY";
            let ready_width = ui.measure(Face::Medium, 10.0, ready) + 16.0 + 9.0;
            ui.rounded(center_x - ready_width * 0.5, ready_y + 4.0, 7.0, 7.0, 3.5, color::hex(0x77d9a2, 255));
            ui.rounded(center_x - ready_width * 0.5 - 5.0, ready_y + 2.0, 17.0, 11.0, 6.0, color::with_alpha(color::hex(0x77d9a2, 255), 0.11));
            ui.text_centered(Face::Bold, 10.0, center_x + 8.0, ready_y, color::muted(), ready);
            let button_x = center_x - button_w * 0.5;
            let button_y = ready_y + 16.0 + 22.0;
            let image_w = button_w * 540.0 / 456.0;
            let image_h = button_h * 252.0 / 168.0;
            ui.sprite(
                "horizon:gui/launcher-play-button",
                button_x - button_w * 42.0 / 456.0,
                button_y - button_h * 40.0 / 168.0,
                image_w,
                image_h,
            );
            let icon_x = button_x + (button_w - 184.0) * 0.5;
            let icon_y = button_y + (button_h - 46.0) * 0.5;
            ui.rounded(icon_x, icon_y, 46.0, 46.0, 14.0, horizon_client::hud::rgba(35, 4, 46, 49));
            ui.border(icon_x, icon_y, 46.0, 46.0, 14.0, horizon_client::hud::rgba(255, 255, 255, 56));
            ui.sprite_tinted("horizon:gui/icons/play", icon_x + 10.5, icon_y + 10.5, 25.0, 25.0, horizon_client::hud::rgba(255, 255, 255, 255));
            let copy_x = icon_x + 61.0;
            ui.text(Face::Bold, 20.0, copy_x, button_y + 20.0, horizon_client::hud::rgba(255, 255, 255, 255), "Play");
            ui.text(Face::Medium, 10.0, copy_x, button_y + 46.0, horizon_client::hud::rgba(255, 255, 255, 199), &format!("Version {}", horizon_core::VANILLA_VERSION));
            self.targets.push((ui.screen_rect([button_x, button_y, button_w, button_h]), Action::Play));
        } else if page.is_catalog() {
            panel(&mut ui, [18.0, 126.0, w - 36.0, 154.0], 16.0, true);
            ui.text(
                Face::Medium,
                18.0,
                32.0,
                142.0,
                color::text(),
                if page == Page::Mods {
                    "Horizon Mods"
                } else if page == Page::Shaders {
                    if w < 360.0 { "Shaders" } else { "Shader Packs" }
                } else {
                    "Resource Packs"
                },
            );
            button(
                &mut ui,
                &mut self.targets,
                [32.0, 174.0, w - 240.0, 44.0],
                &format!(
                    "{}{}",
                    "Search: ",
                    self.catalog.query
                ),
                Action::Field("search"),
                false,
            );
            button(
                &mut ui,
                &mut self.targets,
                [w - 190.0, 174.0, 158.0, 44.0],
                "Search",
                Action::Search,
                true,
            );
            let hint = match page {
                Page::Mods => "Horizon Mods",
                Page::Shaders => "Modrinth shaders · Compatibility varies by pack",
                _ => "Modrinth resource packs",
            };
            let hint = ui.fit_text(Face::Regular, 13.0, hint, w - 64.0);
            ui.text(Face::Regular, 13.0, 32.0, 270.0, color::muted(), &hint);
            let kind = match page {
                Page::Mods => horizon_client::content_files::Kind::Mods,
                Page::Shaders => horizon_client::content_files::Kind::Shaders,
                _ => horizon_client::content_files::Kind::Packs,
            };
            let file_button_width = ((w - 72.0) / 2.0).min(158.0);
            button(
                &mut ui,
                &mut self.targets,
                [32.0, 222.0, file_button_width, 38.0],
                "Open folder",
                Action::OpenFolder(kind),
                false,
            );
            button(
                &mut ui,
                &mut self.targets,
                [w - 32.0 - file_button_width, 222.0, file_button_width, 38.0],
                if page == Page::Shaders { "Import shader" } else { "Import file" },
                Action::Import(kind),
                false,
            );
            let top = 296.0;
            let bottom = h - 112.0;
            self.catalog.scroll = self.catalog.scroll.clamp(
                0.0,
                (self.catalog.results.len() as f32 * 78.0 - (bottom - top)).max(0.0),
            );
            self.list_view = Some([
                top,
                bottom,
                (self.catalog.results.len() as f32 * 78.0 - (bottom - top)).max(0.0),
            ]);
            for (i, p) in self.catalog.results.iter().enumerate() {
                let y = top + i as f32 * 78.0 - self.catalog.scroll;
                let download = self.catalog.download_label(p);
                let download_progress = self.catalog.download_progress(p);
                if download.is_none()
                    && crate::catalog::provider_supported(p)
                    && !crate::catalog::source_only_mod(p)
                {
                    self.list_targets.push((ui.screen_rect([w - 150.0, y, 118.0, 70.0]), Action::Install(i)));
                }
                if y < top || y + 70.0 > bottom {
                    continue;
                }
                panel(&mut ui, [24.0, y, w - 48.0, 68.0], 12.0, false);
                let title = ui.fit_text(Face::Medium, 19.0, &p.title, w - 214.0);
                let summary = ui.fit_text(Face::Regular, 14.0, &p.summary, w - 214.0);
                ui.text(Face::Medium, 19.0, 32.0, y, color::text(), &title);
                ui.text(Face::Regular, 14.0, 32.0, y + 29.0, color::muted(), &summary);
                if let Some(state) = download {
                    if download_progress.is_some() {
                        ui.rounded(
                            w - 165.0,
                            y + 17.0,
                            6.0,
                            6.0,
                            3.0,
                            color::accent(),
                        );
                    }
                    ui.text(Face::Medium, 16.0, w - 150.0, y + 12.0, color::text(), &state);
                    if let Some(progress) = download_progress {
                        let track_x = 32.0;
                        let track_width = w - 64.0;
                        let track_y = y + 57.0;
                        ui.rounded(
                            track_x,
                            track_y,
                            track_width,
                            3.0,
                            1.5,
                            color::border(),
                        );
                        ui.rounded(
                            track_x,
                            track_y,
                            track_width * progress,
                            3.0,
                            1.5,
                            color::accent(),
                        );
                        let shimmer_width = (track_width * 0.18).clamp(18.0, 42.0);
                        let shimmer_x = track_x
                            + (animation * 110.0)
                                .rem_euclid(track_width + shimmer_width)
                            - shimmer_width;
                        let shimmer_left = shimmer_x.max(track_x);
                        let shimmer_right = (shimmer_x + shimmer_width).min(track_x + track_width);
                        if shimmer_right > shimmer_left {
                            ui.rounded(
                                shimmer_left,
                                track_y,
                                shimmer_right - shimmer_left,
                                3.0,
                                1.5,
                                color::with_alpha(color::accent(), 0.65),
                            );
                        }
                    }
                } else if p.kind == crate::browser::Kind::Mod
                    && p.source == crate::browser::SourceId::Modrinth
                {
                    ui.text(
                        Face::Regular,
                        14.0,
                        w - 150.0,
                        y + 12.0,
                        color::muted(),
                        "Java mod",
                    );
                } else if crate::catalog::source_only_mod(p) {
                    ui.text(
                        Face::Regular,
                        14.0,
                        w - 150.0,
                        y + 12.0,
                        color::muted(),
                        ".hnmod: later",
                    );
                } else if p.kind == crate::browser::Kind::Mod && !cfg!(target_pointer_width = "64")
                {
                    ui.text(
                        Face::Regular,
                        14.0,
                        w - 150.0,
                        y + 12.0,
                        color::muted(),
                        "64-bit required",
                    );
                } else {
                    button(
                        &mut ui,
                        &mut self.targets,
                        [w - 150.0, y, 118.0, 42.0],
                        self.catalog.install_button(p),
                        Action::Install(i),
                        false,
                    );
                }
            }
            button(
                &mut ui,
                &mut self.targets,
                [32.0, h - 100.0, 120.0, 40.0],
                "Previous",
                Action::Previous,
                false,
            );
            button(
                &mut ui,
                &mut self.targets,
                [166.0, h - 100.0, 120.0, 40.0],
                "Next",
                Action::Next,
                false,
            );
        } else if page == Page::Installed {
            use horizon_client::content_files::Kind;
            let top = 156.0;
            let bottom = h - 100.0;
            let row_height = if compact { 110.0 } else { 72.0 };
            let total = Kind::ALL
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    48.0 + if self.catalog.sections[i] {
                        self.catalog.local.iter().filter(|e| e.kind == *k).count() as f32
                            * row_height
                    } else {
                        0.0
                    }
                })
                .sum::<f32>();
            let maximum = (total - (bottom - top)).max(0.0);
            self.catalog.scroll = self.catalog.scroll.clamp(0.0, maximum);
            self.list_view = Some([top, bottom, maximum]);
            let mut offset = 0.0;
            for (section, kind) in Kind::ALL.into_iter().enumerate() {
                let count = self.catalog.local.iter().filter(|e| e.kind == kind).count();
                let y = top + offset - self.catalog.scroll;
                let label = format!(
                    "{} {} ({count})",
                    if self.catalog.sections[section] {
                        "-"
                    } else {
                        "+"
                    },
                    kind.label()
                );
                self.list_targets.push((ui.screen_rect([32.0, y, w - 210.0, 40.0]), Action::ToggleSection(section)));
                self.list_targets.push((ui.screen_rect([w - 165.0, y, 133.0, 40.0]), Action::Import(kind)));
                if y >= top && y + 40.0 <= bottom {
                    button(
                        &mut ui,
                        &mut self.targets,
                        [32.0, y, w - 210.0, 40.0],
                        &label,
                        Action::ToggleSection(section),
                        false,
                    );
                    button(
                        &mut ui,
                        &mut self.targets,
                        [w - 165.0, y, 133.0, 40.0],
                        "Import",
                        Action::Import(kind),
                        false,
                    );
                }
                offset += 48.0;
                if !self.catalog.sections[section] {
                    continue;
                }
                for (i, item) in self
                    .catalog
                    .local
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.kind == kind)
                {
                    let y = top + offset - self.catalog.scroll;
                    offset += row_height;
                    self.list_targets.push((
                        ui.screen_rect([
                            if compact { 32.0 } else { w - 300.0 },
                            y,
                            120.0,
                            row_height - 4.0,
                        ]),
                        Action::ToggleLocal(i),
                    ));
                    self.list_targets.push((
                        ui.screen_rect([w - 165.0, y, 133.0, row_height - 4.0]),
                        Action::DeleteLocal(i),
                    ));
                    if y < top || y + row_height - 4.0 > bottom {
                        continue;
                    }
                    panel(&mut ui, [24.0, y, w - 48.0, row_height - 4.0], 12.0, false);
                    let title = ui.fit_text(
                        Face::Medium,
                        19.0,
                        &item.title,
                        if compact { w - 64.0 } else { w - 364.0 },
                    );
                    ui.text(Face::Medium, 19.0, 32.0, y, color::text(), &title);
                    ui.text(
                        Face::Regular,
                        14.0,
                        32.0,
                        y + 28.0,
                        color::muted(),
                        kind.label(),
                    );
                    if kind == Kind::Mods && !cfg!(target_pointer_width = "64") {
                        ui.text(
                            Face::Regular,
                            14.0,
                            if compact { 32.0 } else { w - 300.0 },
                            y + if compact { 62.0 } else { 12.0 },
                            color::muted(),
                            "64-bit required",
                        );
                    } else {
                        toggle(
                            &mut ui,
                            &mut self.targets,
                            [
                                if compact { 32.0 } else { w - 300.0 },
                                y + if compact { 55.0 } else { 0.0 },
                                120.0,
                                40.0,
                            ],
                            item.enabled,
                            Action::ToggleLocal(i),
                        );
                    }
                    button(
                        &mut ui,
                        &mut self.targets,
                        [w - 165.0, y + if compact { 55.0 } else { 0.0 }, 133.0, 40.0],
                        "Remove",
                        Action::DeleteLocal(i),
                        false,
                    );
                }
            }
        } else {
            let c = &self.catalog.config;
            ui.text(
                Face::Medium,
                24.0,
                32.0,
                156.0,
                color::text(),
                "Launch settings",
            );
            let row_height = if compact { 82.0 } else { 66.0 };
            self.catalog.scroll = self
                .catalog
                .scroll
                .clamp(0.0, (14.0 * row_height - (h - 130.0 - 195.0)).max(0.0));
            self.list_view = Some([
                195.0,
                h - 130.0,
                (14.0 * row_height - (h - 130.0 - 195.0)).max(0.0),
            ]);
            for (i, (title, value, action)) in [
                ("Rendering", c.renderer.label().to_owned(), Action::Renderer),
                (
                    "Client resolution",
                    if self.catalog.editing == Some("render_scale") {
                        self.catalog.render_scale_text.clone()
                    } else {
                        format!("{}%", c.render_scale.percent())
                    },
                    Action::RenderScale,
                ),
                ("Profile", format!("Profile {}", c.profile), Action::Profile),
                (
                    "Orientation",
                    c.orientation.label().to_owned(),
                    Action::Orientation,
                ),
                ("Input", c.input_host.label().to_owned(), Action::InputHost),
                (
                    "Immersive fullscreen",
                    if c.screen.immersive { "On" } else { "Off" }.into(),
                    Action::Immersive,
                ),
                (
                    "Avoid camera cutout",
                    if c.screen.avoid_cutout { "On" } else { "Off" }.into(),
                    Action::Cutout,
                ),
                (
                    "Aspect ratio",
                    c.screen.aspect.label().into(),
                    Action::Aspect,
                ),
                (
                    "Display refresh rate",
                    c.device.display_label(),
                    Action::DisplayHz,
                ),
                (
                    "Performance hints",
                    if c.device.performance_hints {
                        "On (ADPF)"
                    } else {
                        "Off"
                    }
                    .into(),
                    Action::PerformanceHints,
                ),
                (
                    "GPU profiling",
                    if c.device.gpu_profile {
                        "On / included in report"
                    } else {
                        "Off"
                    }
                    .into(),
                    Action::GpuProfile,
                ),
                (
                    "Performance recording",
                    if c.record_performance {
                        "On / up to 30 min"
                    } else {
                        "Off"
                    }
                    .into(),
                    Action::RecordPerformance,
                ),
                (
                    "Performance report",
                    "Open in Files / Reports".into(),
                    Action::ExportPerformance,
                ),
                (
                    "Server",
                    if c.server.is_empty() {
                        "Singleplayer / tap to enter server".into()
                    } else {
                        c.server.clone()
                    },
                    Action::Field("server"),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let y = 195.0 + i as f32 * row_height - self.catalog.scroll;
                self.list_targets.push((
                    ui.screen_rect([
                        if compact { 32.0 } else { w * 0.36 },
                        y,
                        if compact { w - 64.0 } else { w * 0.64 - 32.0 },
                        if compact { 70.0 } else { 46.0 },
                    ]),
                    action,
                ));
                if y < 185.0 || y + if compact { 70.0 } else { 46.0 } > h - 130.0 {
                    continue;
                }
                panel(&mut ui, [24.0, y - 2.0, w - 48.0, row_height - 4.0], 12.0, false);
                ui.text(
                    Face::Regular,
                    16.0,
                    32.0,
                    y + if compact { 0.0 } else { 13.0 },
                    color::muted(),
                    title,
                );
                if matches!(
                    action,
                    Action::PerformanceHints | Action::Immersive | Action::Cutout
                ) {
                    toggle(
                        &mut ui,
                        &mut self.targets,
                        [
                            if compact { 32.0 } else { w * 0.36 },
                            y + if compact { 24.0 } else { 0.0 },
                            if compact { w - 64.0 } else { w * 0.64 - 32.0 },
                            46.0,
                        ],
                        match action {
                            Action::PerformanceHints => c.device.performance_hints,
                            Action::Immersive => c.screen.immersive,
                            Action::Cutout => c.screen.avoid_cutout,
                            _ => c.device.gpu_profile,
                        },
                        action,
                    );
                } else {
                    button(
                        &mut ui,
                        &mut self.targets,
                        [
                            if compact { 32.0 } else { w * 0.36 },
                            y + if compact { 24.0 } else { 0.0 },
                            if compact { w - 64.0 } else { w * 0.64 - 32.0 },
                            46.0,
                        ],
                        &format!(
                            "{} {}",
                            value,
                            if matches!(
                                action,
                                Action::Renderer
                                    | Action::Aspect
                                    | Action::DisplayHz
                                    | Action::RenderScale
                                    | Action::Profile
                                    | Action::Orientation
                                    | Action::InputHost
                            ) {
                                "v"
                            } else {
                                ""
                            }
                        ),
                        action,
                        false,
                    );
                }
                ui.rounded(
                    32.0,
                    y + if compact { 78.0 } else { 62.0 },
                    w - 64.0,
                    1.0,
                    0.0,
                    color::border(),
                );
            }
            button(
                &mut ui,
                &mut self.targets,
                [w * 0.5 + 6.0, h - 114.0, w * 0.5 - 38.0, 42.0],
                "Microsoft / Xbox account",
                Action::Account,
                true,
            );
            button(
                &mut ui,
                &mut self.targets,
                [32.0, h - 114.0, w * 0.5 - 38.0, 42.0],
                "Startup diagnostics",
                Action::Logs,
                false,
            );
        }
        let status = self.error.as_deref().unwrap_or(&self.catalog.status);
        for (i, line) in status
            .chars()
            .collect::<Vec<_>>()
            .chunks(110)
            .take(2)
            .enumerate()
        {
            ui.text(
                Face::Regular,
                14.0,
                32.0,
                h - 47.0 + i as f32 * 18.0,
                color::muted(),
                &ui.fit_text(
                    Face::Regular,
                    14.0,
                    &line.iter().collect::<String>(),
                    (w - 210.0).max(40.0),
                ),
            );
        }
        if self.login.visible {
            self.list_targets.clear();
            self.list_view = None;
            self.login.draw(
                &mut ui,
                &mut self.targets,
                &self.catalog.config.name,
                self.catalog.editing == Some("name"),
            );
        }
        if let Some(chooser) = &mut self.chooser {
            chooser.draw(&mut ui, &mut self.targets);
            self.list_targets = chooser.navigation(ui.w).into_iter().map(|(rect, action)| (ui.screen_rect(rect), action)).collect();
            self.list_view = Some(chooser.viewport(ui.h));
        }
        if let Some([top, bottom, maximum]) = self.list_view
            && maximum > 0.0
            && bottom > top
        {
            let scroll = self
                .chooser
                .as_ref()
                .map_or(self.catalog.scroll, |c| c.scroll);
            let height = bottom - top;
            let thumb = (height * height / (height + maximum)).max(20.0).min(height);
            let y = top + (height - thumb) * (scroll / maximum).clamp(0.0, 1.0);
            ui.rounded(w - 12.0, top, 4.0, height, 2.0, color::border());
            ui.rounded(w - 12.0, y, 4.0, thumb, 2.0, color::muted());
        }
        let caret_visible = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .is_ok_and(|t| t.as_millis() % 1000 < 500);
        if caret_visible && let Some(name) = self.catalog.editing {
            let value = match name {
                "name" => self.catalog.config.name.clone(),
                "server" => self.catalog.config.server.clone(),
                "render_scale" => self.catalog.render_scale_text.clone(),
                _ => format!("Search: {}", self.catalog.query),
            };
            for ([x, y, w, _], action) in &self.targets {
                if *action == Action::Field(name)
                    || (name == "render_scale" && *action == Action::RenderScale)
                {
                    let local = ui.local_rect([*x, *y, *w, 0.0]);
                    let text = ui.fit_text(Face::Medium, 16.0, &value, w - 24.0);
                    let caret_x = local[0] + 12.0 + ui.measure(Face::Medium, 16.0, &text);
                    ui.rounded(
                        caret_x.min(local[0] + w - 10.0),
                        local[1] + 12.0,
                        1.0,
                        18.0,
                        0.0,
                        color::text(),
                    );
                }
            }
        }
        if let Some(action) = self.pending_focus.take() {
            self.focus = self.targets.iter().position(|(_, a)| *a == action);
        }
        if let Some(index) = self.focus
            && let Some(([x, y, w, h], _)) = self.targets.get(index)
        {
            let local = ui.local_rect([*x, *y, *w, *h]);
            ui.border(local[0] - 2.0, local[1] - 2.0, local[2] + 4.0, local[3] + 4.0, 2.0, color::text());
            self.cursor = (
                f64::from((x + w / 2.0) * ui.density),
                f64::from((y + h / 2.0) * ui.density),
            );
        }
        if self.mouse_down || self.drag.as_ref().is_some_and(|drag| !drag.moved) {
            let point = [
                self.cursor.0 as f32 / ui.density,
                self.cursor.1 as f32 / ui.density,
            ];
            if let Some(([x, y, w, h], _)) = self.targets.iter().rev().find(|(rect, _)| {
                point[0] >= rect[0]
                    && point[0] <= rect[0] + rect[2]
                    && point[1] >= rect[1]
                    && point[1] <= rect[1] + rect[3]
            }) {
                let local = ui.local_rect([*x, *y, *w, *h]);
                ui.rounded(local[0], local[1], local[2], local[3], 2.0, horizon_client::hud::rgba(255, 255, 255, 48));
            }
        }
        if !self.mouse_down
            && self.drag.is_none()
            && let Some((action, time)) = self.feedback
            && time.elapsed() < std::time::Duration::from_millis(160)
            && let Some(([x, y, w, h], _)) = self.targets.iter().find(|(_, a)| *a == action)
        {
            let local = ui.local_rect([*x, *y, *w, *h]);
            ui.rounded(local[0], local[1], local[2], local[3], 2.0, horizon_client::hud::rgba(255, 255, 255, 48));
        }
        ui.set_origin(0.0, 0.0);
        ui.w = screen_w;
        crate::documents::draw_ui(&mut ui);
        ui.finish();

        if !r.render(&game::loading_frame(&b))? && !b.patches.is_empty() {
            // A suspended/lost surface skips the frame, including its glyph
            // uploads. Rebuild them next time instead of caching invisible text.
            horizon_client::ui_theme::reset_atlas();
        }
        Ok(())
    }
    pub(super) fn activate(&mut self, events: &ActiveEventLoop) -> Result<()> {
        if self.session_worker.is_some() {
            return Ok(());
        }
        let Some(r) = &self.renderer else {
            return Ok(());
        };
        let mut b = HudBatch::new([r.config.width, r.config.height]);
        let ui = Ui::mobile(&self.gui.as_ref().unwrap().atlas, &mut b, (0.0, 0.0));
        let p = self.focus.and_then(|index| self.targets.get(index)).map_or(
            [
                self.cursor.0 as f32 / ui.density,
                self.cursor.1 as f32 / ui.density,
            ],
            |([x, y, w, h], _)| [x + w / 2.0, y + h / 2.0],
        );
        let action = self
            .targets
            .iter()
            .find(|([x, y, w, h], _)| {
                (*x..*x + *w).contains(&p[0]) && (*y..*y + *h).contains(&p[1])
            })
            .map(|(_, a)| *a);
        if self.catalog.busy() || crate::documents::busy() {
            return Ok(());
        }
        self.feedback = action.map(|action| (action, std::time::Instant::now()));
        self.focus = None;
        if action.is_some() && horizon_client::official_install::ready(&self.home) {
            let audio = self.audio.get_or_insert_with(|| {
                let mut audio = horizon_client::audio::Audio::new(
                    &self.home.join(".local/horizon-client/26.3"),
                );
                let settings = horizon_client::settings::Settings::load(
                    &self
                        .catalog
                        .config
                        .game_home(&self.home)
                        .join("options.txt"),
                );
                audio.master = settings.volumes[0];
                let mut volumes = settings.volumes;
                volumes[0] = 1.0;
                horizon_client::settings::set_source_volumes(volumes);
                audio
            });
            audio.play_button_click();
        }
        match action {
            Some(Action::ToggleDrawer) => {
                self.drawer_open = !self.drawer_open;
            }
            Some(Action::CloseDrawer) => {
                self.drawer_open = false;
            }
            Some(Action::ToggleTheme) => {
                let light = !color::is_light();
                color::set_light(light);
                horizon_client::ui_theme::set_preference(
                    &self.home.join("installer"),
                    "theme",
                    Some(if light { "light" } else { "dark" }),
                );
            }
            Some(Action::Immersive) => {
                self.catalog.config.screen.immersive ^= true;
                self.catalog.config.save(&self.home)?;
                self.configure_screen();
            }
            Some(Action::Cutout) => {
                self.catalog.config.screen.avoid_cutout ^= true;
                self.catalog.config.save(&self.home)?;
                self.configure_screen();
            }
            Some(Action::Aspect) => self.open_chooser(chooser::Kind::Aspect),
            Some(Action::ChooseAspect(aspect)) => {
                self.catalog.config.screen.aspect = aspect;
                self.catalog.config.save(&self.home)?;
                self.configure_screen();
                self.chooser = None;
            }
            Some(Action::DisplayHz) => self.open_chooser(chooser::Kind::DisplayHz),
            Some(Action::ChooseDisplayHz(hz)) => {
                self.catalog.config.device.display_hz = hz;
                self.catalog.config.device.sanitize();
                self.catalog.config.save(&self.home)?;
                crate::device_runtime::configure(self.catalog.config.device);
                self.chooser = None;
            }
            Some(Action::PerformanceHints) => {
                self.catalog.config.device.performance_hints ^= true;
                self.catalog.status="ADPF gives Android CPU frame-work hints on supported devices. It does not cap FPS or guarantee a speedup.".into();
                self.catalog.config.save(&self.home)?;
                crate::device_runtime::configure(self.catalog.config.device);
            }
            Some(Action::GpuProfile) => {
                self.catalog.config.device.gpu_profile ^= true;
                self.catalog.status="Detailed GPU profiling records diagnostics and has overhead. Reports remain on this device.".into();
                self.catalog.config.save(&self.home)?;
                crate::device_runtime::configure(self.catalog.config.device);
            }

            Some(Action::OpenFolder(kind)) => {
                crate::platform::hide();
                let folder = match kind {
                    horizon_client::content_files::Kind::Mods => "mods",
                    horizon_client::content_files::Kind::Packs => "packs",
                    horizon_client::content_files::Kind::Shaders => "shaders",
                };
                let id = crate::document_store::folder_id(self.catalog.config.profile, folder);
                self.catalog.status = if crate::platform::open_documents(&id) {
                    "Opened in Files. Return here after copying files.".into()
                } else {
                    "Open the system Files app and choose Horizon Android.".into()
                };
            }
            Some(Action::Import(kind)) => {
                crate::platform::hide();
                let home = self.catalog.config.game_home(&self.home);
                if let Err(e) = crate::documents::begin(kind, home) {
                    self.catalog.status = e;
                }
            }
            Some(Action::ToggleSection(i)) => {
                self.catalog.sections[i] = !self.catalog.sections[i];
                self.catalog.scroll = 0.0;
            }
            Some(Action::ToggleLocal(i)) => self.catalog.toggle_local(&self.home, i),
            Some(Action::DeleteLocal(i)) => {
                self.catalog.pending_delete = self.catalog.local.get(i).cloned();
                if let Some(item) = &self.catalog.pending_delete {
                    self.chooser = Some(super::chooser::Chooser::confirm_delete(item.file.clone()));
                }
            }
            Some(Action::ConfirmDeleteLocal) => {
                self.catalog.remove_local(&self.home);
                self.chooser = None;
            }
            Some(Action::Account) => {
                self.login.visible = true;
            }
            Some(Action::CloseLogin) => {
                self.login.hide();
                crate::platform::hide();
                self.catalog.editing = None;
            }
            Some(Action::DeviceSignIn) => self
                .login
                .start_device(&self.catalog.config.microsoft_client_id),
            Some(Action::CopyLoginCode) => self.login.copy_code(),
            Some(Action::CancelLogin) => {
                self.login.close();
                self.login.visible = true;
            }
            Some(Action::ReviewApp) => self.login.review_app(),
            Some(Action::SignIn) => self.login.start(&self.catalog.config.microsoft_client_id),
            Some(Action::SignOut) => self.login.sign_out(),
            Some(Action::LoginLink) => self.login.open_link(),
            Some(Action::Page(page)) => {
                self.drawer_open = false;
                self.catalog.editing = None;
                if crate::platform::active() {
                    crate::platform::hide();
                }
                self.catalog.page = page;
                self.catalog.scroll = 0.0;
                self.error = None;
                self.catalog.offset = 0;
                self.catalog.browse(&self.home);
            }
            Some(Action::Play) => self.action(events)?,
            Some(Action::Search) => {
                crate::platform::hide();
                self.catalog.offset = 0;
                self.catalog.search(&self.home);
            }
            Some(Action::FinishNameEdit) => {
                self.catalog.editing = None;
                crate::platform::hide();
                self.catalog.config.save(&self.home)?;
            }
            Some(Action::Field(name)) => {
                if name == "render_scale" {
                    self.catalog.render_scale_text =
                        self.catalog.config.render_scale.percent().to_string();
                    self.chooser = None;
                }
                self.catalog.editing = Some(name);
                let initial = match name {
                    "name" => &self.catalog.config.name,
                    "server" => &self.catalog.config.server,
                    "render_scale" => &self.catalog.render_scale_text,
                    "client_id" => &self.catalog.config.microsoft_client_id,
                    _ => &self.catalog.query,
                };
                crate::platform::show(initial);
            }
            Some(Action::Install(i)) => self.catalog.install(&self.home, i),
            Some(Action::Next) => {
                self.catalog.offset += 12;
                self.catalog.search(&self.home);
            }
            Some(Action::Previous) => {
                self.catalog.offset = self.catalog.offset.saturating_sub(12);
                self.catalog.search(&self.home);
            }
            Some(Action::Profile) => self.open_chooser(chooser::Kind::Profile),
            Some(Action::Renderer) => self.open_chooser(chooser::Kind::Renderer),
            Some(Action::RenderScale) => self.open_chooser(chooser::Kind::RenderScale),
            Some(Action::ChooseRenderScale(scale)) => {
                self.catalog.config.render_scale = scale;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
                crate::display::set_scale(scale);
            }
            Some(Action::Orientation) => self.open_chooser(chooser::Kind::Orientation),
            Some(Action::InputHost) => self.open_chooser(chooser::Kind::InputHost),
            Some(Action::ChooseInputHost(mode)) => {
                self.catalog.config.input_host = mode;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
                crate::configure_input_mode(mode);
            }
            Some(Action::ChooseOrientation(mode)) => {
                self.catalog.config.orientation = mode;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
                crate::platform::set_orientation(mode);
            }
            Some(Action::ChooseRenderer(mode)) => {
                self.catalog.config.renderer = mode;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
            }
            Some(Action::ChooseProfile(profile)) => {
                std::fs::create_dir_all(if profile == 0 {
                    self.home.clone()
                } else {
                    self.home
                        .join("profiles")
                        .join(format!("profile-{profile}"))
                })?;
                self.catalog.config.profile = profile;
                self.catalog.config.save(&self.home)?;
                self.catalog.refresh(&self.home);
                self.chooser = None;
            }
            Some(Action::Dismiss) => self.chooser = None,
            Some(Action::Logs) => {
                self.catalog.status = std::fs::read_to_string(self.home.join("android-error.txt"))
                    .unwrap_or_else(|_| {
                        "No startup error. In game: F3 shows device, terrain and frame diagnostics."
                            .into()
                    });
            }
            Some(Action::RecordPerformance) => {
                self.catalog.config.record_performance = !self.catalog.config.record_performance;
                if !self.catalog.config.record_performance {
                    crate::performance::stop();
                }
                self.catalog.config.save(&self.home)?;
                self.catalog.status = if self.catalog.config.record_performance {
                    "Recording starts on Play. Detailed timings add overhead. Return here to save the report."
                } else { "Performance recording off." }.into();
            }
            Some(Action::ExportPerformance) => {
                self.catalog.status = match crate::performance::export(&self.home) {
                    Ok(()) => "Reports opened in Files. Copy or share a report from there.".into(),
                    Err(e) => e,
                };
            }
            None => {}
        }
        Ok(())
    }
    pub(super) fn ui_density(&self) -> f32 {
        let (Some(r), Some(gui)) = (&self.renderer, &self.gui) else {
            return 1.0;
        };
        let mut batch = HudBatch::new([r.config.width, r.config.height]);
        Ui::mobile(&gui.atlas, &mut batch, (0.0, 0.0)).density
    }

    pub(super) fn touch_event(&mut self, events: &ActiveEventLoop, t: Touch) -> Result<()> {
        self.focus = None;
        self.pending_focus = None;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        self.cursor = (t.location.x, t.location.y);
        match t.phase {
            TouchPhase::Started => {
                self.drag = Some(super::gesture::Drag::new(t.location.x, t.location.y));
            }
            TouchPhase::Moved => {
                let density = self.ui_density() as f64;
                if let Some(drag) = &mut self.drag {
                    let delta = drag.update(t.location.x, t.location.y, density);
                    if drag.moved {
                        let sidebar_origin = drag.origin_x() / density;
                        let sidebar_y = drag.origin_y() / density;
                        if let Some([top, bottom, maximum]) = self.sidebar_view
                            && sidebar_origin < self.sidebar_width as f64
                            && sidebar_y >= top as f64
                            && sidebar_y <= bottom as f64
                        {
                            self.sidebar_scroll = (self.sidebar_scroll + delta).clamp(0.0, maximum);
                        } else if let Some(chooser) = &mut self.chooser {
                            chooser.scroll = (chooser.scroll + delta).max(0.0);
                        } else if self
                            .list_view
                            .is_some_and(|view| drag.origin_y() / density >= view[0] as f64)
                        {
                            self.catalog.scroll = (self.catalog.scroll + delta).max(0.0);
                        }
                    }
                }
            }
            TouchPhase::Ended => {
                if self.drag.take().is_some_and(|drag| !drag.moved) {
                    self.activate(events)?;
                }
            }
            TouchPhase::Cancelled => {
                self.drag = None;
            }
        }
        Ok(())
    }
    pub(super) fn type_key(&mut self, key: KeyEvent) {
        if key.physical_key == PhysicalKey::Code(KeyCode::Escape)
            || key.logical_key
                == winit::keyboard::Key::Named(winit::keyboard::NamedKey::BrowserBack)
        {
            if self.drawer_open {
                self.drawer_open = false;
                return;
            }
            if self.login.visible {
                self.login.hide();
                crate::platform::hide();
                self.catalog.editing = None;
                return;
            }
            if self.chooser.take().is_some() {
                return;
            }
            crate::platform::hide();
            self.catalog.editing = None;
            let _ = self.catalog.config.save(&self.home);
            return;
        }
        if self.catalog.editing.is_none() {
            return;
        }
        let out = match key.physical_key {
            PhysicalKey::Code(KeyCode::Backspace) => Some(Output::Backspace),
            PhysicalKey::Code(KeyCode::Enter) => {
                crate::platform::hide();
                Some(Output::Enter)
            }
            _ => key.text.map(|s| Output::Text(s.into())),
        };
        if let Some(out) = out {
            self.catalog.edit(out, &self.home);
        }
    }
}
