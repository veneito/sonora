use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, AvailableSpace, Context, Div, Entity, FocusHandle, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, Task, Window, WindowControlArea, canvas, deferred, div, point,
    size,
};
use i18n::t;
use input::{CloseWindow, ShowMainWindow, WORKSPACE_CONTEXT};
use state::{Cover, Playback, Queue, SideTab, Sonora};
use ui::{
    ActiveTheme as _, Artwork, Button, Scrubber, ScrubberState, TabBar, Text, clock, tabular,
};

use crate::chrome::Aside;
use crate::root::ui_font;
use crate::shared::transport::{fraction, next, percent, previous, toggle, volume_icon};

const REST: Duration = Duration::from_secs(2);
const VOLUME_CLOSE_DELAY: Duration = Duration::from_millis(200);

/// A separate view of the shared player with its own lyrics scroll position.
pub struct MiniPlayer {
    playback: Entity<Playback>,
    queue: Entity<Queue>,
    cover: Entity<Cover>,
    aside: Entity<Aside>,
    panel: Option<SideTab>,
    seek: ScrubberState,
    pending: Option<f32>,
    volume: ScrubberState,
    muted: Option<f32>,
    over_volume: bool,
    over_volume_zone: bool,
    over_volume_panel: bool,
    volume_open: bool,
    volume_close: Option<Task<()>>,
    volume_held: bool,
    settled: bool,
    rest: Option<Task<()>>,
    focus: FocusHandle,
    grabbed: bool,
}

impl MiniPlayer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sonora = Sonora::global(cx);
        let playback = sonora.playback.clone();
        let queue = sonora.queue.clone();
        let cover = sonora.cover.clone();
        let settings = sonora.settings.clone();
        let devices = sonora.devices.clone();
        cx.observe(&playback, |_, _, cx| cx.notify()).detach();
        cx.observe(&queue, |_, _, cx| cx.notify()).detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();
        cx.observe(&devices, |_, _, cx| cx.notify()).detach();
        cx.observe(&cover, |_, _, cx| cx.notify()).detach();
        let aside = cx.new(|cx| {
            let mut lyrics = Aside::new(queue.clone(), playback.clone(), SideTab::Lyrics, cx);
            lyrics.strip();
            lyrics
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut player = Self {
            playback,
            queue,
            cover,
            aside,
            panel: None,
            seek: ScrubberState::new("mini-seek"),
            pending: None,
            volume: ScrubberState::new("mini-volume"),
            muted: None,
            over_volume: false,
            over_volume_zone: false,
            over_volume_panel: false,
            volume_open: false,
            volume_close: None,
            volume_held: false,
            settled: false,
            rest: None,
            focus,
            grabbed: false,
        };
        player.wake(cx);
        player
    }

    /// Restarts the artwork's idle delay. An active scrubber or volume popup keeps controls up.
    fn wake(&mut self, cx: &mut Context<Self>) {
        if self.settled {
            self.settled = false;
            cx.notify();
        }
        self.rest = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REST).await;
            this.update(cx, |this, cx| {
                this.rest = None;
                if this.panel.is_none()
                    && this.pending.is_none()
                    && !this.grabbed
                    && !this.volume_open
                    && !this.volume_held
                {
                    this.settled = true;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Bridges the pointer's move between the button and popup without closing mid-crossing.
    fn follow_volume(&mut self, cx: &mut Context<Self>) {
        self.volume_close = None;
        if self.over_volume || self.over_volume_zone || self.over_volume_panel || self.volume_held {
            self.volume_open = true;
        } else {
            self.volume_close = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(VOLUME_CLOSE_DELAY).await;
                this.update(cx, |this, cx| {
                    this.volume_close = None;
                    if this.over_volume
                        || this.over_volume_zone
                        || this.over_volume_panel
                        || this.volume_held
                    {
                        return;
                    }
                    this.volume_open = false;
                    this.wake(cx);
                    cx.notify();
                })
                .ok();
            }));
        }
        self.wake(cx);
        cx.notify();
    }

    fn track_details(&self, cx: &App) -> Div {
        let theme = *cx.theme();
        let track = self.playback.read(cx).shown_track(cx);
        let title = track.as_ref().map_or_else(
            || t!("player-nothing-playing"),
            |track| track.name.clone().into(),
        );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(div().truncate().child(title))
            .when_some(track, |this, track| {
                this.child(
                    div()
                        .truncate()
                        .text_size(theme.text(Text::Small))
                        .text_color(theme.muted_foreground)
                        .child(track.artists),
                )
            })
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .p_2()
            .child(
                div()
                    .id("mini-drag")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h(theme.metrics.control_small)
                    .items_center()
                    .gap_2()
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.grabbed = true),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.grabbed = false;
                            this.wake(cx);
                        }),
                    )
                    .on_mouse_down_out(cx.listener(|this, _, _, _| this.grabbed = false))
                    .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                        if this.grabbed {
                            this.grabbed = false;
                            window.start_window_move();
                        }
                    }))
                    .child(self.track_details(cx)),
            )
            .child(
                Button::new("mini-main")
                    .ghost()
                    .small()
                    .icon("icons/maximize.svg")
                    .tooltip("mini-player-main")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowMainWindow), cx)),
            )
            .child(
                Button::new("mini-close")
                    .ghost()
                    .small()
                    .icon("icons/x.svg")
                    .tooltip("app-close-window")
                    .on_click(|_, window, _| window.remove_window()),
            )
    }

    fn modes(&self, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let tab = |id, icon, hint, panel| {
            let selected = self.panel == panel;
            Button::new(id)
                .ghost()
                .small()
                .icon(icon)
                .tooltip_above(hint)
                .selected(selected)
                .rounded(theme.radius)
                .tint(match selected {
                    true => theme.foreground,
                    false => theme.muted_foreground,
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.panel = panel;
                    if let Some(tab) = panel {
                        this.aside.update(cx, |aside, cx| aside.show(tab, cx));
                    }
                    this.wake(cx);
                    cx.notify();
                }))
        };
        div().flex().flex_none().justify_center().p_2().child(
            TabBar::new("mini-modes")
                .rounded(theme.radius + theme.metrics.pad * 0.25)
                .items([
                    tab(
                        "mini-artwork",
                        "icons/disc-3.svg",
                        "fullscreen-artwork",
                        None,
                    ),
                    tab(
                        "mini-lyrics",
                        "icons/mic-vocal.svg",
                        "lyrics-title",
                        Some(SideTab::Lyrics),
                    ),
                    tab(
                        "mini-queue",
                        "icons/list-music.svg",
                        "queue-title",
                        Some(SideTab::Queue),
                    ),
                ]),
        )
    }

    /// Uses the same large cover as Now Playing while preserving embedded local artwork.
    fn artwork(&self, fill: bool, cx: &App) -> impl IntoElement {
        let theme = *cx.theme();
        let cover = self.playback.read(cx).shown_track(cx).and_then(|track| {
            let local = track.id.as_deref().is_some_and(music::is_local_id)
                || track
                    .cover
                    .as_deref()
                    .is_some_and(|url| url.starts_with("file://"));
            match local {
                true => track.cover,
                false => track
                    .album_id
                    .as_deref()
                    .and_then(|album| self.cover.read(cx).large_for(album))
                    .map(str::to_owned)
                    .or(track.cover),
            }
        });
        canvas(
            move |bounds, window, cx| {
                let side = match fill {
                    true => bounds.size.width.max(bounds.size.height),
                    false => (bounds.size.width.min(bounds.size.height) - theme.metrics.pad * 2.)
                        .max(Pixels::ZERO),
                };
                let mut artwork = Artwork::new(cover)
                    .size(side)
                    .flex_none()
                    .when(fill, |this| this.corner_radius(Pixels::ZERO))
                    .into_any_element();
                artwork.layout_as_root(
                    size(
                        AvailableSpace::Definite(side),
                        AvailableSpace::Definite(side),
                    ),
                    window,
                    cx,
                );
                artwork.prepaint_at(
                    bounds.origin
                        + point(
                            (bounds.size.width - side) / 2.,
                            (bounds.size.height - side) / 2.,
                        ),
                    window,
                    cx,
                );
                artwork
            },
            |_, mut artwork, window, cx| artwork.paint(window, cx),
        )
        .size_full()
    }

    fn timeline(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let player = self.playback.read(cx);
        let (elapsed, total) = player.shown_time(cx);
        let position = self.pending.unwrap_or_else(|| fraction(elapsed, total));
        let enabled = player.shown_track(cx).is_some() && !total.is_zero();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .text_size(theme.text(Text::Tiny))
            .text_color(theme.muted_foreground)
            .font_features(tabular())
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(clock(total.mul_f32(position))),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Scrubber::new(&self.seek, position)
                        .colors(theme.progress_bar, theme.muted, theme.foreground)
                        .enabled(enabled)
                        .on_move(cx.listener(|this, position: &f32, _, cx| {
                            this.pending = Some(*position);
                            this.wake(cx);
                            cx.notify();
                        }))
                        .on_release(cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if let Some(position) = this.pending.take() {
                                this.playback
                                    .update(cx, |player, cx| player.seek_fraction(position, cx));
                            }
                            this.wake(cx);
                            cx.notify();
                        })),
                ),
            )
            .child(div().flex_none().whitespace_nowrap().child(clock(total)))
    }

    /// Keeps volume in a hover popup so the timeline can use the window's full width.
    fn sound(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let level = self.playback.read(cx).shown_volume(cx);
        let open = self.volume_open;
        div()
            .id("mini-sound")
            .relative()
            .flex_none()
            .on_hover(cx.listener(|this, hovering: &bool, _, cx| {
                this.over_volume = *hovering;
                this.follow_volume(cx);
            }))
            .child(
                Button::new("mini-mute")
                    .ghost()
                    .small()
                    .icon(volume_icon(level))
                    .when(!open, |this| {
                        this.tooltip_above(match level <= 0.001 {
                            true => "player-unmute",
                            false => "player-mute",
                        })
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        let current = this.playback.read(cx).shown_volume(cx);
                        let level = match current <= 0.001 {
                            true => this.muted.take().unwrap_or(0.7),
                            false => {
                                this.muted = Some(current);
                                0.
                            }
                        };
                        this.playback
                            .update(cx, |player, cx| player.set_volume(level, cx));
                    })),
            )
            .when(open, |this| {
                this.child(deferred(
                    div()
                        .id("mini-volume-zone")
                        .absolute()
                        .bottom_0()
                        .left(-theme.metrics.pad)
                        .w(theme.metrics.control_small + theme.metrics.pad * 2.)
                        .pb(theme.metrics.control_small)
                        .pt_2()
                        .on_hover(cx.listener(|this, hovering: &bool, _, cx| {
                            this.over_volume_zone = *hovering;
                            this.follow_volume(cx);
                        }))
                        .child(
                            div()
                                .id("mini-volume-panel")
                                .occlude()
                                .flex()
                                .justify_center()
                                .p_2()
                                .bg(theme.popover)
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .on_hover(cx.listener(|this, hovering: &bool, _, cx| {
                                    this.over_volume_panel = *hovering;
                                    this.follow_volume(cx);
                                }))
                                .child(
                                    div().h(theme.metrics.row * 2.).flex().child(
                                        Scrubber::new(&self.volume, level)
                                            .vertical()
                                            .colors(
                                                theme.progress_bar,
                                                theme.muted,
                                                theme.foreground,
                                            )
                                            .when(self.volume_held, |this| {
                                                this.bubble(level, percent(level))
                                            })
                                            .on_move(cx.listener(|this, level: &f32, _, cx| {
                                                this.volume_held = true;
                                                this.muted = None;
                                                this.playback.update(cx, |player, cx| {
                                                    player.set_volume(*level, cx)
                                                });
                                                this.follow_volume(cx);
                                            }))
                                            .on_release(cx.listener(
                                                |this, _: &MouseUpEvent, _, cx| {
                                                    this.volume_held = false;
                                                    this.follow_volume(cx);
                                                },
                                            )),
                                    ),
                                ),
                        ),
                ))
            })
    }

    fn controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_3()
            .p_2()
            .child(previous(&self.playback, false, cx))
            .child(toggle(&self.playback, false, false, cx))
            .child(next(&self.playback, &self.queue, false, cx))
            .child(self.sound(cx))
    }
}

impl Render for MiniPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        window.set_rem_size(theme.font_size);
        let font = ui_font(Sonora::global(cx).settings.read(cx).font());
        let artwork_only = self.panel.is_none() && self.settled;
        let root = div()
            .id("mini-player")
            .relative()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(font)
            .text_size(theme.font_size)
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_hover(cx.listener(|this, _: &bool, _, cx| this.wake(cx)))
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| this.wake(cx)))
            .on_key_down(cx.listener(|this, _, _, cx| this.wake(cx)))
            .when(artwork_only, |this| this.child(self.artwork(true, cx)))
            .when(!artwork_only, |this| {
                this.child(self.header(cx))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .when(self.panel.is_some(), |this| this.child(self.aside.clone()))
                            .when(self.panel.is_none(), |this| {
                                this.child(self.artwork(false, cx))
                            }),
                    )
                    .child(self.modes(cx))
                    .child(self.timeline(cx))
                    .child(self.controls(cx))
            });
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let root = root.child(ui::WindowFrame::new());
        root
    }
}
