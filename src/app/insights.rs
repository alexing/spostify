//! AI insights in the application: the panel's requests and answers, and the
//! API key in Settings. The feature itself lives in `crate::insights`; this
//! is only where it meets the app's state.

use super::*;
use crate::insights::{self, KeyStatus, Mode, Subject};

impl App {
    pub(crate) fn apply_insights(&mut self, action: insights::Action, ctx: &egui::Context) {
        match action {
            insights::Action::Open { uri, name, track } => {
                if let Some(subject) = self.insight_subject(&uri, &name, track.as_deref()) {
                    self.request_insight(subject, Mode::Quick, false);
                }
            }
            insights::Action::Regenerate => {
                if let Some(panel) = &self.insights.panel {
                    let (subject, mode) = (panel.subject.clone(), panel.mode);
                    self.request_insight(subject, mode, true);
                }
            }
            insights::Action::Deepen => {
                if let Some(panel) = &self.insights.panel {
                    let subject = panel.subject.clone();
                    self.request_insight(subject, Mode::Deep, false);
                }
            }
            insights::Action::Close => self.insights.panel = None,
            insights::Action::CheckKey => {
                if self.insights.key == KeyStatus::Unknown {
                    self.send_insights_job(insights::Job::CheckKey);
                }
            }
            insights::Action::SaveKey => {
                let key = std::mem::take(&mut self.insights.key_draft);
                if !key.trim().is_empty() {
                    self.send_insights_job(insights::Job::StoreKey(insights::Secret(key)));
                }
            }
            insights::Action::ForgetKey => self.send_insights_job(insights::Job::ForgetKey),
            insights::Action::OpenSettings => {
                self.insights.panel = None;
                self.open(Page::Settings);
                crate::ui::insights::reveal_setting(ctx, self.locale);
            }
        }
    }

    pub(crate) fn receive_insights(&mut self, outcome: insights::Outcome) {
        match outcome {
            insights::Outcome::Progress { id, progress } => self.insights.progress(id, progress),
            insights::Outcome::Generated { id, result } => {
                if let Err(error) = &result {
                    log::info!("AI insight unavailable: {error}");
                }
                self.insights.finish(id, result);
            }
            insights::Outcome::Key(status) => self.insights.key = status,
        }
    }

    /// Key work goes to the backend; the demo has none, and no key.
    fn send_insights_job(&mut self, job: insights::Job) {
        if self.offline {
            self.insights.key = KeyStatus::Missing;
            return;
        }
        self.insights.key = KeyStatus::Checking;
        self.backend.send(Command::Insights(Box::new(job)));
    }

    fn request_insight(&mut self, subject: Subject, mode: Mode, force: bool) {
        let Some(request) = self.insights.begin(subject, self.locale, mode, force) else {
            return;
        };
        if self.offline {
            // The demo plays a sample in as if it were being written.
            #[cfg(any(test, feature = "demo"))]
            self.insights.play_demo(
                request.id,
                insights::demo::insight(&request.subject, mode),
                std::time::Instant::now(),
            );
            #[cfg(not(any(test, feature = "demo")))]
            self.insights
                .finish(request.id, Err(insights::Error::Offline));
            return;
        }
        self.backend
            .send(Command::Insights(Box::new(insights::Job::Generate(
                Box::new(request),
            ))));
    }

    /// Describes the item at `uri` with the most the app knows about it: a
    /// loaded page first, then the library and search results, then the
    /// row's own song, and at least its name.
    pub(crate) fn insight_subject(
        &self,
        uri: &str,
        name: &str,
        track: Option<&Track>,
    ) -> Option<Subject> {
        let id = util::uri_id(uri)?;
        match insights::Kind::from_uri(uri)? {
            insights::Kind::Album => {
                let page = self.album_pages.get(id);
                let tracks = page.map(|page| page.tracks.items.as_slice());
                page.and_then(|page| page.album.get())
                    .or_else(|| self.known_album(id))
                    .map(|album| Subject::from_album(album, tracks.unwrap_or_default()))
            }
            insights::Kind::Artist => {
                let page = self.artist_pages.get(id);
                let top = page
                    .and_then(|page| page.top_tracks.get())
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                page.and_then(|page| page.artist.get())
                    .or_else(|| self.known_artist(id))
                    .map(|artist| Subject::from_artist(artist, top))
            }
            insights::Kind::Track => track
                .filter(|track| track.id.as_deref() == Some(id))
                .or_else(|| self.track_cache.get(id))
                .and_then(Subject::from_track),
        }
        .or_else(|| Subject::from_uri(uri, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insights::Status;

    fn demo_app() -> App {
        let root = std::env::temp_dir().join(format!(
            "spotifast-insights-app-test-{}",
            std::process::id()
        ));
        let mut app = App::new(
            &crate::backend::Waker::default(),
            AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        crate::demo::populate(&mut app);
        app
    }

    #[test]
    fn opening_an_album_uses_its_loaded_details() {
        let app = demo_app();
        let album = app.known_album("alb0").cloned().expect("demo album");
        let subject = app
            .insight_subject(&album.uri, "ignored", None)
            .expect("subject");
        assert_eq!(subject.kind, insights::Kind::Album);
        assert_eq!(subject.name, album.name);
        assert!(!subject.artists.is_empty());
    }

    #[test]
    fn an_unknown_item_is_still_described_by_its_name() {
        let app = demo_app();
        let subject = app
            .insight_subject("spotify:artist:nowhere", "Somebody", None)
            .unwrap();
        assert_eq!(subject.name, "Somebody");
        assert!(
            app.insight_subject("spotify:playlist:x", "List", None)
                .is_none()
        );
    }

    #[test]
    fn the_demo_answers_with_a_sample_without_the_network() {
        let mut app = demo_app();
        let ctx = egui::Context::default();
        app.apply(
            Action::Insights(insights::Action::Open {
                uri: "spotify:album:alb0".into(),
                name: "Album".into(),
                track: None,
            }),
            &ctx,
        );
        let panel = app.insights.panel.as_ref().expect("panel open");
        assert!(matches!(&panel.status, Status::Loading { .. }));
        assert_eq!(panel.mode, Mode::Quick);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(3600);
        assert!(!app.insights.tick_demo(later));
        let panel = app.insights.panel.as_ref().unwrap();
        assert!(
            matches!(&panel.status, Status::Ready(insight) if insight.markdown.contains("Sample"))
        );
        // Digging deeper plays the longer sample.
        app.apply(Action::Insights(insights::Action::Deepen), &ctx);
        assert_eq!(app.insights.panel.as_ref().unwrap().mode, Mode::Deep);
        assert!(!app.insights.tick_demo(later));
        let panel = app.insights.panel.as_ref().unwrap();
        assert!(
            matches!(&panel.status, Status::Ready(insight) if insight.mode == Mode::Deep && insight.markdown.contains("## Sources"))
        );
        // Regenerating keeps the mode shown.
        app.apply(Action::Insights(insights::Action::Regenerate), &ctx);
        assert_eq!(app.insights.panel.as_ref().unwrap().mode, Mode::Deep);
        app.apply(Action::Insights(insights::Action::Close), &ctx);
        assert!(app.insights.panel.is_none());
    }

    #[test]
    fn the_demo_reports_no_key_and_keeps_typed_keys_out_of_state() {
        let mut app = demo_app();
        let ctx = egui::Context::default();
        app.apply(Action::Insights(insights::Action::CheckKey), &ctx);
        assert_eq!(app.insights.key, KeyStatus::Missing);
        app.insights.key_draft = "sk-ant-test".into();
        app.apply(Action::Insights(insights::Action::SaveKey), &ctx);
        assert!(app.insights.key_draft.is_empty());
    }
}
