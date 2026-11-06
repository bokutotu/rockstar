use super::{LoadState, MediaKind, MediaSession, PlaybackMode};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, Render, Role, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Window,
    component::{
        ActiveTheme as _, Disableable as _, WindowExt as _,
        button::Button,
        notification::Notification,
        slider::{Slider, SliderEvent, SliderState},
    },
    div, img,
    prelude::FluentBuilder as _,
    px, relative,
};

pub(crate) struct MediaView {
    pub session: Entity<MediaSession>,
    focus: FocusHandle,
    embedded: bool,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    seeking: bool,
    changing_volume: bool,
}

impl MediaView {
    pub fn new(
        session: Entity<MediaSession>,
        embedded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001));
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(1.)
                .step(0.01)
                .default_value(1.)
        });
        let id = cx.entity_id();
        cx.on_release(move |this, cx| {
            this.session
                .update(cx, |session, cx| session.set_visible(id, false, cx));
        })
        .detach();
        cx.observe_in(&session, window, |this, _, window, cx| {
            if let Some(playback) = this.session.read(cx).playback.clone() {
                if !this.seeking {
                    let percent = playback.duration.filter(|d| !d.is_zero()).map_or(0., |d| {
                        (playback.position.as_secs_f64() / d.as_secs_f64()).clamp(0., 1.) as f32
                    });
                    if this.seek.read(cx).value().start() != percent {
                        this.seek
                            .update(cx, |slider, cx| slider.set_value(percent, window, cx));
                    }
                }
                if !this.changing_volume && this.volume.read(cx).value().start() != playback.volume
                {
                    this.volume.update(cx, |slider, cx| {
                        slider.set_value(playback.volume, window, cx)
                    });
                }
            }
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&seek, window, |this, _, event, window, cx| match event {
            SliderEvent::Change(_) => this.seeking = true,
            SliderEvent::Release(value) => {
                this.seeking = false;
                let duration = this
                    .session
                    .read(cx)
                    .playback
                    .as_ref()
                    .and_then(|p| p.duration);
                if let Some(duration) = duration {
                    let result = this.session.update(cx, |session, _| {
                        session.seek(duration.mul_f64(f64::from(value.start().clamp(0., 1.))))
                    });
                    report(result, window, cx);
                }
            }
        })
        .detach();
        cx.subscribe_in(&volume, window, |this, _, event, window, cx| {
            this.changing_volume = matches!(event, SliderEvent::Change(_));
            let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
            let result = this
                .session
                .update(cx, |session, _| session.set_volume(value.start()));
            report(result, window, cx);
        })
        .detach();
        Self {
            session,
            focus: cx.focus_handle(),
            embedded,
            seek,
            volume,
            seeking: false,
            changing_volume: false,
        }
    }

    pub fn set_visible(&self, visible: bool, cx: &mut Context<Self>) {
        let id = cx.entity_id();
        self.session
            .update(cx, |session, cx| session.set_visible(id, visible, cx));
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if *cx.global::<crate::mode::Mode>() != crate::mode::Mode::Normal {
            cx.set_global(crate::mode::Mode::Normal);
        }
        self.focus.focus(window, cx);
    }
}

impl Focusable for MediaView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MediaView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let kind = session.kind;
        let ready = session.load_state == LoadState::Ready;
        let playing = session
            .playback
            .as_ref()
            .is_some_and(|p| p.mode == PlaybackMode::Playing);
        let can_seek = ready
            && session
                .playback
                .as_ref()
                .is_some_and(|p| p.seekable && p.duration.is_some_and(|d| !d.is_zero()));
        let label = session.source.label();
        let status = match &session.load_state {
            LoadState::Loading => "Loading…".to_string(),
            LoadState::Failed(error) => format!("Cannot open media: {error}"),
            LoadState::Ready => match &session.playback {
                None => "Image".to_string(),
                Some(playback) => format!(
                    "{} · {} / {}",
                    match playback.mode {
                        PlaybackMode::Paused => "Paused",
                        PlaybackMode::Playing => "Playing",
                        PlaybackMode::Ended => "Ended",
                    },
                    time(playback.position),
                    playback
                        .duration
                        .map(time)
                        .unwrap_or_else(|| "--:--".into())
                ),
            },
        };
        let image = session.frame.clone();
        let aspect = image
            .as_ref()
            .map(|image| {
                let size = image.size(0);
                size.width.0 as f32 / size.height.0.max(1) as f32
            })
            .unwrap_or(16. / 9.);
        let visual = div()
            .w_full()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .when(self.embedded, |view| {
                view.aspect_ratio(aspect).max_h(px(480.))
            })
            .when(!self.embedded, |view| view.flex_1())
            .when_some(image, |view, image| {
                view.child(
                    img(image)
                        .id("media-frame")
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            });
        div()
            .id("media-view")
            .role(Role::Group)
            .aria_label(format!("Media {label}"))
            .track_focus(&self.focus)
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .when(!self.embedded, |view| view.h_full())
            .when(self.embedded, |view| {
                view.border_1().border_color(cx.theme().border).rounded_md()
            })
            .when(kind != MediaKind::Audio, |view| view.child(visual))
            .when(kind == MediaKind::Audio, |view| {
                view.child(
                    div()
                        .id("audio-waveform")
                        .role(Role::Image)
                        .aria_label("Audio waveform icon")
                        .w_full()
                        .h(px(44.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .children(
                            [10., 22., 34., 18., 38., 26., 14.]
                                .into_iter()
                                .map(|height| {
                                    div()
                                        .w(px(5.))
                                        .h(px(height))
                                        .rounded_sm()
                                        .bg(cx.theme().primary)
                                }),
                        ),
                )
            })
            .child(
                div()
                    .id("media-status")
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .text_sm()
                    .w_full()
                    .child(status),
            )
            .when(kind != MediaKind::Image, |view| {
                view.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    Button::new("media-play")
                                        .label(if playing { "Pause" } else { "Play" })
                                        .disabled(!ready)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            let result = this
                                                .session
                                                .update(cx, |session, cx| session.toggle_play(cx));
                                            report(result, window, cx);
                                            cx.stop_propagation();
                                        })),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Slider::new(&self.seek)
                                            .disabled(!can_seek)
                                            .accessibility_label("Playback position"),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .max_w(relative(1.))
                                .child("Volume")
                                .child(
                                    div().w(px(120.)).min_w_0().child(
                                        Slider::new(&self.volume)
                                            .disabled(!ready)
                                            .accessibility_label("Media volume"),
                                    ),
                                ),
                        ),
                )
            })
    }
}

fn time(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
fn report(result: anyhow::Result<()>, window: &mut Window, cx: &mut App) {
    if let Err(error) = result {
        window.push_notification(Notification::error(error.to_string()), cx);
    }
}
