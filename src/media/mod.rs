//! Read-only media sessions. Text buffers and Markdown source stay untouched.
pub(crate) mod http;
pub(crate) mod markdown;
mod player;
mod source;
mod view;

use anyhow::{Result, bail};
use gpui_kit::{
    App, AppContext as _, Asset as _, Context, EntityId, ImageAssetLoader, RenderImage, Task,
};
use std::{collections::HashSet, sync::Arc, time::Duration};

pub(crate) use source::{MediaKind, MediaSource, classify_file, resolve_source};
pub(crate) use view::MediaView;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LoadState {
    Loading,
    Ready,
    Failed(String),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PlaybackMode {
    Paused,
    Playing,
    Ended,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlaybackState {
    pub mode: PlaybackMode,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub seekable: bool,
}
impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            mode: PlaybackMode::Paused,
            position: Duration::ZERO,
            duration: None,
            volume: 1.,
            seekable: false,
        }
    }
}

pub(crate) struct MediaSession {
    pub source: MediaSource,
    pub kind: MediaKind,
    pub load_state: LoadState,
    pub playback: Option<PlaybackState>,
    pub frame: Option<Arc<RenderImage>>,
    player: Option<player::Player>,
    task: Option<Task<()>>,
    visible_views: HashSet<EntityId>,
}

impl MediaSession {
    pub fn new(kind: MediaKind, source: MediaSource, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| this.shutdown(cx)).detach();
        cx.on_app_quit(|this, cx| {
            this.task = None;
            let worker = this.player.take().and_then(|mut player| player.shutdown());
            cx.background_spawn(async move {
                if let Some(worker) = worker {
                    let _ = worker.join();
                }
            })
        })
        .detach();
        let mut this = Self {
            source,
            kind,
            load_state: LoadState::Loading,
            playback: (kind != MediaKind::Image).then(PlaybackState::default),
            frame: None,
            player: None,
            task: None,
            visible_views: HashSet::new(),
        };
        this.start(cx);
        this
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.kind == MediaKind::Image {
            let source = self.source.clone();
            let image = ImageAssetLoader::load(source.resource(), cx);
            let timeout = cx.background_executor().timer(Duration::from_secs(15));
            let loading = cx.background_spawn(async move {
                source.validate_file()?;
                futures_lite::future::race(
                    async move { image.await.map_err(anyhow::Error::from) },
                    async move {
                        timeout.await;
                        Err(anyhow::anyhow!("Image loading timed out"))
                    },
                )
                .await
            });
            self.task = Some(cx.spawn(async move |this, cx| {
                let result = loading.await;
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(frame) => {
                            this.frame = Some(frame);
                            this.load_state = LoadState::Ready;
                        }
                        Err(error) => this.load_state = LoadState::Failed(format!("{error:#}")),
                    }
                    cx.notify();
                });
            }));
        } else {
            match player::Player::spawn(self.kind, self.source.clone()) {
                Ok(player) => {
                    let updates = player.updates.clone();
                    self.player = Some(player);
                    self.task = Some(cx.spawn(async move |this, cx| {
                        while updates.recv().await.is_ok() {
                            if this
                                .update(cx, |this, cx| {
                                    let Some(player) = &this.player else { return };
                                    let update = player.take_update();
                                    this.load_state = update.load;
                                    this.playback = Some(update.playback);
                                    if let Some(frame) = update.frame
                                        && let Some(old) = this.frame.replace(frame)
                                    {
                                        cx.drop_image(old, None);
                                    }
                                    cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }));
                }
                Err(error) => self.load_state = LoadState::Failed(error.to_string()),
            }
        }
    }

    pub fn set_visible(&mut self, view: EntityId, visible: bool, cx: &mut Context<Self>) {
        if visible {
            self.visible_views.insert(view);
        } else if self.visible_views.remove(&view) && self.visible_views.is_empty() {
            // Send pause even if a preceding play acknowledgement has not arrived.
            if let Some(player) = &self.player {
                let _ = player.send(player::Command::Pause(true));
            }
            if let Some(playback) = &mut self.playback
                && playback.mode == PlaybackMode::Playing
            {
                playback.mode = PlaybackMode::Paused;
            }
            cx.notify();
        }
    }

    pub fn toggle_play(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let Some(playback) = &self.playback else {
            bail!("Images cannot be played");
        };
        if self.load_state != LoadState::Ready || self.visible_views.is_empty() {
            bail!("Media is not ready or visible");
        }
        let player = self
            .player
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Player has stopped"))?;
        if playback.mode == PlaybackMode::Ended {
            player.send(player::Command::Seek(Duration::ZERO))?;
        }
        player.send(player::Command::Pause(
            playback.mode == PlaybackMode::Playing,
        ))?;
        cx.notify();
        Ok(())
    }

    pub fn seek(&mut self, position: Duration) -> Result<()> {
        let playback = self
            .playback
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Images cannot seek"))?;
        if self.load_state != LoadState::Ready || !playback.seekable {
            bail!("This media cannot seek");
        }
        let position = playback
            .duration
            .map_or(position, |duration| position.min(duration));
        self.player
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Player has stopped"))?
            .send(player::Command::Seek(position))
    }

    pub fn set_volume(&mut self, volume: f32) -> Result<()> {
        if !volume.is_finite() {
            bail!("Invalid volume");
        }
        self.player
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Player has stopped"))?
            .send(player::Command::Volume(volume.clamp(0., 1.)))
    }

    pub fn shutdown(&mut self, cx: &mut App) {
        self.task = None;
        if let Some(mut player) = self.player.take() {
            let worker = player.shutdown();
            cx.background_executor()
                .spawn(async move {
                    if let Some(worker) = worker {
                        let _ = worker.join();
                    }
                })
                .detach();
        }
        if let Some(frame) = self.frame.take() {
            cx.drop_image(frame, None);
        }
    }
}
