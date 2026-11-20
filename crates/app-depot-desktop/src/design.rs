//! Presentation tokens shared by the native desktop screens.
use eframe::egui::{self, Color32, FontFamily, FontId, RichText, TextStyle};

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Color32,
    pub sidebar: Color32,
    pub surface: Color32,
    pub elevated: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub good: Color32,
    pub warning: Color32,
    pub error: Color32,
}

pub fn palette(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode {
        Palette {
            canvas: Color32::from_rgb(18, 22, 29),
            sidebar: Color32::from_rgb(22, 28, 36),
            surface: Color32::from_rgb(29, 36, 46),
            elevated: Color32::from_rgb(37, 46, 58),
            border: Color32::from_rgb(57, 68, 81),
            text: Color32::from_rgb(238, 243, 248),
            muted: Color32::from_rgb(168, 181, 196),
            accent: Color32::from_rgb(83, 169, 231),
            accent_text: Color32::from_rgb(8, 27, 44),
            good: Color32::from_rgb(119, 206, 158),
            warning: Color32::from_rgb(240, 187, 102),
            error: Color32::from_rgb(239, 126, 123),
        }
    } else {
        Palette {
            canvas: Color32::from_rgb(245, 248, 251),
            sidebar: Color32::from_rgb(233, 239, 245),
            surface: Color32::WHITE,
            elevated: Color32::from_rgb(238, 244, 249),
            border: Color32::from_rgb(207, 218, 229),
            text: Color32::from_rgb(31, 43, 57),
            muted: Color32::from_rgb(70, 86, 104),
            accent: Color32::from_rgb(20, 104, 170),
            accent_text: Color32::WHITE,
            good: Color32::from_rgb(29, 123, 71),
            warning: Color32::from_rgb(151, 92, 13),
            error: Color32::from_rgb(177, 56, 53),
        }
    }
}

pub fn install(ctx: &egui::Context, theme: egui::ThemePreference, zoom: f32) {
    ctx.set_theme(theme);
    ctx.set_zoom_factor(zoom.clamp(0.85, 1.6));
    for mode in [egui::Theme::Dark, egui::Theme::Light] {
        let mut style = (*ctx.style_of(mode)).clone();
        let dark = mode == egui::Theme::Dark;
        let color = if dark {
            Color32::from_rgb(18, 22, 29)
        } else {
            Color32::from_rgb(245, 248, 251)
        };
        let surface = if dark {
            Color32::from_rgb(29, 36, 46)
        } else {
            Color32::WHITE
        };
        let border = if dark {
            Color32::from_rgb(57, 68, 81)
        } else {
            Color32::from_rgb(207, 218, 229)
        };
        let text = if dark {
            Color32::from_rgb(238, 243, 248)
        } else {
            Color32::from_rgb(31, 43, 57)
        };
        style
            .text_styles
            .insert(TextStyle::Body, FontId::new(16.0, FontFamily::Proportional));
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(14.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(23.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(14.0, FontFamily::Monospace),
        );
        style.spacing.item_spacing = egui::vec2(11.0, 10.0);
        style.spacing.button_padding = egui::vec2(14.0, 9.0);
        style.spacing.interact_size.y = 36.0;
        style.visuals.window_fill = surface;
        style.visuals.panel_fill = color;
        style.visuals.faint_bg_color = surface;
        style.visuals.extreme_bg_color = if dark {
            Color32::from_rgb(23, 29, 37)
        } else {
            Color32::from_rgb(251, 253, 255)
        };
        style.visuals.override_text_color = Some(text);
        style.visuals.widgets.noninteractive.bg_fill = surface;
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border);
        style.visuals.widgets.inactive.bg_fill = surface;
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
        style.visuals.widgets.hovered.bg_fill = if dark {
            Color32::from_rgb(44, 54, 67)
        } else {
            Color32::from_rgb(223, 235, 245)
        };
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, border);
        style.visuals.widgets.active.bg_fill = if dark {
            Color32::from_rgb(53, 66, 80)
        } else {
            Color32::from_rgb(212, 229, 244)
        };
        for widget in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            widget.corner_radius = egui::CornerRadius::same(9);
            widget.fg_stroke.color = text;
        }
        ctx.set_style_of(mode, style);
    }
}

pub fn primary(label: impl Into<String>, p: Palette) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label.into()).strong().color(p.accent_text))
        .fill(p.accent)
        .stroke(egui::Stroke::NONE)
        .corner_radius(9)
}

pub fn surface(ui: &egui::Ui) -> egui::Frame {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(18))
}

pub fn eyebrow(text: impl Into<String>, p: Palette) -> RichText {
    RichText::new(text.into())
        .size(12.0)
        .strong()
        .color(p.muted)
}
