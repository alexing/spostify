//! AI insights in the interface: the sparkles button and menu item, the
//! briefing panel, and the API key's row in Settings.
//!
//! Everything here draws and pushes `Action::Insights`; the app applies the
//! actions and the backend does the work (`crate::insights`).

use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke};

use crate::api::models::Track;
use crate::app::App;
use crate::i18n::{Locale, gettext};
use crate::insights::markdown::{self, Block, Span};
use crate::insights::{self, Error, Insight, KeyStatus, Kind, Status};
use crate::model::Action;
use crate::theme::{self, Icon, Palette};

use super::widgets;

/// Where to make an Anthropic API key.
const KEYS_URL: &str = "https://console.anthropic.com/settings/keys";

fn label(locale: Locale) -> std::borrow::Cow<'static, str> {
    gettext(locale, "AI insights")
}

fn open(app: &mut App, uri: &str, name: &str, track: Option<&Track>) {
    app.actions.push(Action::Insights(insights::Action::Open {
        uri: uri.to_string(),
        name: name.to_string(),
        track: track.map(|track| Box::new(track.clone())),
    }));
}

/// The sparkles button in an album's or artist's header. Draws nothing for
/// items without briefings, such as playlists.
pub fn button(app: &mut App, ui: &mut egui::Ui, uri: &str, name: &str) {
    if Kind::from_uri(uri).is_none() {
        return;
    }
    let palette = app.palette;
    if theme::icon_button(
        ui,
        Icon::Sparkles,
        24.0,
        palette.secondary,
        palette.text,
        &label(app.locale),
    )
    .clicked()
    {
        open(app, uri, name, None);
    }
}

/// The "AI insights" item in a context menu.
pub fn menu_item(app: &mut App, ui: &mut egui::Ui, uri: &str, name: &str, track: Option<&Track>) {
    if Kind::from_uri(uri).is_none() {
        return;
    }
    let palette = app.palette;
    if widgets::menu_item(ui, &palette, Some(Icon::Sparkles), &label(app.locale)) {
        open(app, uri, name, track);
    }
}

// ---- panel ----------------------------------------------------------------

/// The briefing, over the window, while one is open.
pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(panel) = app.insights.panel.clone() else {
        return;
    };
    let palette = app.palette;
    let locale = app.locale;
    let frame = Frame::new()
        .fill(palette.overlay)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS + 4))
        .inner_margin(Margin::same(24))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 40,
            spread: 0,
            color: palette.shadow,
        });
    let screen = ctx.content_rect();
    let response = egui::Modal::new(egui::Id::new("ai-insights"))
        .frame(frame)
        .backdrop_color(egui::Color32::from_black_alpha(if palette.dark {
            150
        } else {
            80
        }))
        .show(ctx, |ui| {
            ui.set_width((screen.width() - 96.0).clamp(280.0, 600.0));
            header(app, ui, &panel.subject);
            ui.add_space(14.0);
            let body_height = (screen.height() - 260.0).clamp(160.0, 620.0);
            egui::ScrollArea::vertical()
                .id_salt(("ai-insights-body", panel.request))
                .max_height(body_height)
                .auto_shrink([false, true])
                .show(ui, |ui| match &panel.status {
                    Status::Loading => loading(ui, &palette, locale, &panel.subject.name),
                    Status::Ready(insight) => {
                        render(app, ui, &insight.markdown);
                        if insight.truncated {
                            ui.add_space(8.0);
                            note(
                                ui,
                                palette.warning,
                                &gettext(locale, "The answer was cut short."),
                            );
                        }
                    }
                    Status::Failed(error) => failure(app, ui, error),
                });
            ui.add_space(14.0);
            footer(app, ui, &panel.status);
        });
    if response.should_close() {
        app.actions.push(Action::Insights(insights::Action::Close));
    }
}

fn header(app: &mut App, ui: &mut egui::Ui, subject: &insights::Subject) {
    let palette = app.palette;
    let locale = app.locale;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        theme::icon(ui, Icon::Sparkles, 22.0, palette.accent);
        ui.vertical(|ui| {
            let kind = match subject.kind {
                Kind::Album => gettext(locale, "Album"),
                Kind::Artist => gettext(locale, "Artist"),
                Kind::Track => gettext(locale, "Song"),
            };
            theme::text(
                ui,
                format!("{} \u{b7} {kind}", label(locale)),
                theme::semibold(12.0),
                palette.secondary,
            );
            ui.horizontal(|ui| {
                ui.set_max_width(ui.available_width() - 40.0);
                theme::text(ui, &subject.name, theme::bold(20.0), palette.text);
            });
            if !subject.artists.is_empty() {
                theme::text(
                    ui,
                    subject.artists.join(", "),
                    theme::regular(13.0),
                    palette.secondary,
                );
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            if theme::icon_button(
                ui,
                Icon::X,
                18.0,
                palette.secondary,
                palette.text,
                &gettext(locale, "Close"),
            )
            .clicked()
            {
                app.actions.push(Action::Insights(insights::Action::Close));
            }
        });
    });
}

fn loading(ui: &mut egui::Ui, palette: &Palette, locale: Locale, name: &str) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        theme::spinner(ui, 28.0, palette.accent);
        ui.add_space(10.0);
        // Translators: {name} is an album, artist or song name.
        let title = gettext(locale, "Researching “{name}”…").replace("{name}", name);
        theme::text(ui, title, theme::semibold(15.0), palette.text);
        ui.add_space(4.0);
        wrapped(
            ui,
            &gettext(
                locale,
                "Claude is checking MusicBrainz credits and the web. This can take a minute.",
            ),
            theme::regular(13.0),
            palette.secondary,
        );
    });
    ui.add_space(24.0);
}

/// What went wrong, and the way forward.
pub fn error_message(locale: Locale, error: &Error) -> String {
    match error {
        Error::MissingKey => gettext(
            locale,
            "AI insights are written by Claude with your own Anthropic API key. Add one in Settings to start.",
        )
        .into_owned(),
        Error::InvalidKey => gettext(
            locale,
            "Anthropic rejected the API key. Check it in Settings.",
        )
        .into_owned(),
        Error::RateLimited => gettext(
            locale,
            "Anthropic is limiting requests from this key. Try again in a minute.",
        )
        .into_owned(),
        Error::Overloaded => {
            gettext(locale, "Anthropic is busy right now. Try again shortly.").into_owned()
        }
        Error::Refused => gettext(locale, "Claude declined to write about this item.").into_owned(),
        Error::Empty => gettext(locale, "Claude returned an empty answer. Try again.").into_owned(),
        Error::Offline => gettext(locale, "AI insights need a network connection.").into_owned(),
        // Translators: {error} is a network error message.
        Error::Network(detail) => gettext(locale, "Couldn't reach Anthropic: {error}")
            .replace("{error}", detail),
        Error::Api { status, message } => {
            let detail = if message.is_empty() {
                status.to_string()
            } else {
                format!("{status}, {message}")
            };
            // Translators: {error} is the error Anthropic reported.
            gettext(locale, "Anthropic returned an error: {error}").replace("{error}", &detail)
        }
    }
}

fn failure(app: &mut App, ui: &mut egui::Ui, error: &Error) {
    let palette = app.palette;
    let locale = app.locale;
    ui.add_space(16.0);
    ui.horizontal_top(|ui| {
        theme::icon(ui, Icon::CircleAlert, 18.0, palette.warning);
        wrapped(
            ui,
            &error_message(locale, error),
            theme::regular(14.0),
            palette.text,
        );
    });
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if matches!(error, Error::MissingKey | Error::InvalidKey) {
            if theme::pill_button(ui, &palette, &gettext(locale, "Open Settings"), true).clicked() {
                app.actions
                    .push(Action::Insights(insights::Action::OpenSettings));
            }
        } else if theme::pill_button(ui, &palette, &gettext(locale, "Try again"), true).clicked() {
            app.actions
                .push(Action::Insights(insights::Action::Regenerate));
        }
    });
    ui.add_space(8.0);
}

fn footer(app: &mut App, ui: &mut egui::Ui, status: &Status) {
    let palette = app.palette;
    let locale = app.locale;
    ui.horizontal(|ui| {
        let note_text = match status {
            Status::Ready(insight) => written_on(locale, insight),
            _ => String::new(),
        };
        theme::text(ui, note_text, theme::regular(12.0), palette.dim);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ready = matches!(status, Status::Ready(_));
            if ready
                && theme::soft_button(
                    ui,
                    &palette,
                    Some(Icon::Refresh),
                    &gettext(locale, "Regenerate"),
                    false,
                )
                .clicked()
            {
                app.actions
                    .push(Action::Insights(insights::Action::Regenerate));
            }
        });
    });
    ui.add_space(6.0);
    wrapped(
        ui,
        &gettext(
            locale,
            "Written by Claude from MusicBrainz credits and web search. It can make mistakes.",
        ),
        theme::regular(11.5),
        palette.dim,
    );
}

/// "Written 2026-10-08", or "Saved 2026-10-08" for one from the cache.
fn written_on(locale: Locale, insight: &Insight) -> String {
    let date = jiff::Timestamp::from_second(insight.created_at)
        .map(|at| {
            at.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default();
    if insight.cached {
        // Translators: {date} is a date in YYYY-MM-DD form.
        gettext(locale, "Saved {date}").replace("{date}", &date)
    } else {
        // Translators: {date} is a date in YYYY-MM-DD form.
        gettext(locale, "Written {date}").replace("{date}", &date)
    }
}

fn wrapped(ui: &mut egui::Ui, text: &str, font: egui::FontId, color: egui::Color32) {
    ui.add(egui::Label::new(RichText::new(text).font(font).color(color)).wrap());
}

fn note(ui: &mut egui::Ui, color: egui::Color32, text: &str) {
    ui.horizontal_top(|ui| {
        theme::icon(ui, Icon::Info, 14.0, color);
        wrapped(ui, text, theme::regular(12.5), color);
    });
}

// ---- markdown ---------------------------------------------------------------

/// The briefing's Markdown, in the panel's type.
fn render(app: &mut App, ui: &mut egui::Ui, text: &str) {
    let palette = app.palette;
    let mut first = true;
    for block in markdown::parse(text) {
        match block {
            Block::Heading { level, spans } => {
                if !first {
                    ui.add_space(12.0);
                }
                let size = match level {
                    1 => 18.0,
                    2 => 16.0,
                    _ => 14.5,
                };
                let font = theme::bold(size);
                spans_line(app, ui, &spans, &font, palette.text);
                ui.add_space(4.0);
            }
            Block::Paragraph(spans) => {
                spans_line(app, ui, &spans, &theme::regular(14.0), palette.text);
                ui.add_space(6.0);
            }
            Block::Bullet { depth, spans } => {
                let marker = if depth == 0 { "\u{2022}" } else { "\u{25e6}" };
                item(app, ui, f32::from(depth) * 16.0, marker, &spans);
            }
            Block::Numbered { number, spans } => {
                item(app, ui, 0.0, &format!("{number}."), &spans);
            }
            Block::Quote(spans) => {
                ui.horizontal_top(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), Sense::hover());
                    ui.painter().rect_filled(rect, 1.0, palette.outline);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        spans_line(app, ui, &spans, &theme::regular(14.0), palette.secondary);
                    });
                });
                ui.add_space(6.0);
            }
            Block::Rule => {
                ui.add_space(6.0);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, palette.outline);
                ui.add_space(6.0);
            }
        }
        first = false;
    }
}

fn item(app: &mut App, ui: &mut egui::Ui, indent: f32, marker: &str, spans: &[Span]) {
    let palette = app.palette;
    ui.horizontal_top(|ui| {
        ui.add_space(indent + 2.0);
        ui.allocate_ui(egui::vec2(16.0, 18.0), |ui| {
            theme::text(ui, marker, theme::semibold(14.0), palette.secondary);
        });
        ui.vertical(|ui| {
            spans_line(app, ui, spans, &theme::regular(14.0), palette.text);
        });
    });
    ui.add_space(3.0);
}

/// Runs of text that wrap together, with links that open in the browser.
fn spans_line(
    app: &mut App,
    ui: &mut egui::Ui,
    spans: &[Span],
    font: &egui::FontId,
    color: egui::Color32,
) {
    let palette = app.palette;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 2.0);
        for span in spans {
            match span {
                Span::Text { text, bold, italic } => {
                    let font = if *bold {
                        theme::bold(font.size)
                    } else {
                        font.clone()
                    };
                    let mut rich = RichText::new(text).font(font).color(color);
                    if *italic {
                        rich = rich.italics();
                    }
                    ui.add(egui::Label::new(rich).wrap());
                }
                Span::Code(text) => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(text)
                                .font(egui::FontId::monospace(font.size - 1.0))
                                .color(color)
                                .background_color(palette.surface),
                        )
                        .wrap(),
                    );
                }
                Span::Link { text, url } => {
                    let response = ui
                        .add(
                            egui::Label::new(
                                RichText::new(text)
                                    .font(font.clone())
                                    .color(palette.accent)
                                    .underline(),
                            )
                            .wrap()
                            .sense(Sense::click()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(url.as_str());
                    if response.clicked() {
                        app.actions.push(Action::OpenUrl(url.clone()));
                    }
                }
            }
        }
    });
}

// ---- settings ---------------------------------------------------------------

/// Opens Settings filtered to the API key's row.
pub(crate) fn reveal_setting(ctx: &egui::Context, locale: Locale) {
    super::settings::search_for(ctx, gettext(locale, "Anthropic API key").into_owned());
}

/// The AI insights section of Settings. Returns whether it was shown,
/// for the page's "no settings match" state.
pub fn settings_section(app: &mut App, ui: &mut egui::Ui, needle: &str) -> bool {
    let palette = app.palette;
    let locale = app.locale;
    let title = label(locale);
    let row = gettext(locale, "Anthropic API key");
    let description = gettext(
        locale,
        "Claude writes the insights with your own key. Each new insight is a paid request to Anthropic. The key is kept in the system credential store.",
    );
    if !super::settings::row_matches(needle, &title, &description)
        && !super::settings::row_matches(needle, &row, "")
    {
        return false;
    }
    if app.insights.key == KeyStatus::Unknown {
        app.actions
            .push(Action::Insights(insights::Action::CheckKey));
    }
    super::settings::section(ui, &palette, &title, |ui| {
        widgets::setting_row(ui, &palette, &row, &description, |_| {});
        ui.add_space(6.0);
        let status = match app.insights.key {
            KeyStatus::Unknown | KeyStatus::Checking => gettext(locale, "Checking…"),
            KeyStatus::Stored => gettext(locale, "A key is saved in the system credential store."),
            KeyStatus::Environment => {
                gettext(locale, "Using the ANTHROPIC_API_KEY environment variable.")
            }
            KeyStatus::Missing => gettext(locale, "No key yet."),
            KeyStatus::StoreFailed => gettext(
                locale,
                "The system credential store is unavailable. Unlock it, or set ANTHROPIC_API_KEY.",
            ),
        };
        let status_color = match app.insights.key {
            KeyStatus::Stored | KeyStatus::Environment => palette.accent,
            KeyStatus::StoreFailed => palette.warning,
            _ => palette.secondary,
        };
        theme::text(ui, status, theme::regular(13.0), status_color);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let response = Frame::new()
                .fill(palette.surface)
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    widgets::text_edit(
                        ui,
                        locale,
                        egui::TextEdit::singleline(&mut app.insights.key_draft)
                            .id(egui::Id::new("anthropic-api-key"))
                            .password(true)
                            .hint_text(RichText::new("sk-ant-…").color(palette.dim))
                            .font(theme::regular(13.0))
                            .frame(egui::Frame::NONE)
                            .desired_width(240.0),
                    )
                })
                .inner;
            ui.ctx()
                .accesskit_node_builder(response.id, |node| node.set_label(row.as_ref()));
            let typed = !app.insights.key_draft.trim().is_empty();
            let submitted =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let save = ui
                .add_enabled_ui(typed, |ui| {
                    theme::pill_button(ui, &palette, &gettext(locale, "Save key"), true)
                })
                .inner
                .clicked();
            if typed && (save || submitted) {
                app.actions
                    .push(Action::Insights(insights::Action::SaveKey));
            }
            if app.insights.key == KeyStatus::Stored
                && theme::pill_button(ui, &palette, &gettext(locale, "Remove key"), false).clicked()
            {
                app.actions
                    .push(Action::Insights(insights::Action::ForgetKey));
            }
        });
        ui.add_space(8.0);
        if theme::link(
            ui,
            gettext(locale, "Create a key in the Anthropic Console"),
            theme::regular(13.0),
            palette.accent,
        )
        .clicked()
        {
            app.actions.push(Action::OpenUrl(KEYS_URL.to_string()));
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_a_message_and_none_is_empty() {
        for error in [
            Error::MissingKey,
            Error::InvalidKey,
            Error::RateLimited,
            Error::Overloaded,
            Error::Refused,
            Error::Empty,
            Error::Offline,
            Error::Network("timed out".into()),
            Error::Api {
                status: 400,
                message: "bad request".into(),
            },
        ] {
            let message = error_message(Locale::English, &error);
            assert!(!message.is_empty());
            assert!(!message.contains('{'), "{message}");
        }
        assert!(
            error_message(Locale::English, &Error::Network("timed out".into()))
                .contains("timed out")
        );
    }

    #[test]
    fn dates_say_whether_the_insight_was_saved() {
        let insight = Insight {
            markdown: String::new(),
            created_at: 1_790_000_000,
            truncated: false,
            cached: true,
        };
        assert!(written_on(Locale::English, &insight).starts_with("Saved 2026-"));
        let fresh = Insight {
            cached: false,
            ..insight
        };
        assert!(written_on(Locale::English, &fresh).starts_with("Written 2026-"));
    }
}
