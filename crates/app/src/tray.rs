//! The tray icon, as a StatusNotifierItem, which KDE, Cinnamon, XFCE and Ubuntu's GNOME
//! show, and other GNOMEs with the AppIndicator extension. Its menu lists what's
//! firing; the window has the rest.

use std::f64::consts::PI;

use gtk::cairo;
use ksni::menu::{MenuItem, StandardItem};

use crate::Command;

/// What the icon says: the worst thing happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    /// Nothing firing.
    Quiet,
    /// Warnings only: an amber square and how many.
    Warnings(usize),
    /// Something critical: a red dot and how many alerts are firing in all.
    Critical(usize),
    /// The server can't be asked, or hasn't answered yet: grey, so quiet is never
    /// mistaken for fine.
    Unknown,
}

/// One firing alert, as the menu lists it.
#[derive(Debug, Clone)]
pub struct MenuAlert {
    pub label: String,
    pub path: String,
}

pub struct Tray {
    pub headline: String,
    pub icons: Vec<ksni::Icon>,
    pub firing: Vec<MenuAlert>,
    /// Whether there's a server to open.
    pub connected: bool,
    pub commands: async_channel::Sender<Command>,
}

/// More than this many alerts in the menu is a list nobody reads; the window has
/// them all.
const MOST_IN_MENU: usize = 10;

impl Tray {
    fn send(&self, command: Command) {
        let _ = self.commands.send_blocking(command);
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        crate::APP_ID.into()
    }

    fn title(&self) -> String {
        format!("MaxNines: {}", self.headline)
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "MaxNines".into(),
            description: self.headline.clone(),
            ..Default::default()
        }
    }

    /// Clicking the icon, where the desktop passes it on rather than opening the menu.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: self.headline.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ];
        if !self.firing.is_empty() {
            items.push(MenuItem::Separator);
            for alert in self.firing.iter().take(MOST_IN_MENU) {
                let path = alert.path.clone();
                items.push(
                    StandardItem {
                        // An underscore marks a mnemonic in a menu label.
                        label: alert.label.replace('_', "__"),
                        activate: Box::new(move |tray: &mut Self| {
                            tray.send(Command::Open(path.clone()));
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            if self.firing.len() > MOST_IN_MENU {
                items.push(
                    StandardItem {
                        label: format!("and {} more", self.firing.len() - MOST_IN_MENU),
                        activate: Box::new(|tray: &mut Self| tray.send(Command::Show)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Show MaxNines Desktop".into(),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Show)),
                ..Default::default()
            }
            .into(),
        );
        if self.connected {
            items.push(
                StandardItem {
                    label: "Open MaxNines in the browser".into(),
                    activate: Box::new(|tray: &mut Self| tray.send(Command::Open("/".into()))),
                    ..Default::default()
                }
                .into(),
            );
            items.push(
                StandardItem {
                    label: "Refresh".into(),
                    activate: Box::new(|tray: &mut Self| tray.send(Command::Refresh)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// The icon at the sizes panels ask for most.
pub fn icons(state: IconState) -> Vec<ksni::Icon> {
    [22, 32, 48]
        .into_iter()
        .filter_map(|size| icon(state, size))
        .collect()
}

/// MaxNines' mark, a 9 in a rounded square, in its green or grey, with a badge for
/// what's firing. Drawn rather than read from a file, so the count can be in it.
fn icon(state: IconState, size: i32) -> Option<ksni::Icon> {
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).ok()?;
    {
        let context = cairo::Context::new(&surface).ok()?;
        let scale = f64::from(size) / 32.0;
        context.scale(scale, scale);
        let (r, g, b) = if state == IconState::Unknown {
            (0.55, 0.58, 0.61)
        } else {
            (0.435, 0.863, 0.549)
        };
        rounded_square(&context, 2.0, 2.0, 28.0, 7.0);
        context.set_source_rgb(r, g, b);
        context.fill().ok()?;
        draw_nine(
            &context,
            if state == IconState::Unknown {
                0.18
            } else {
                0.024
            },
        );
        match state {
            IconState::Critical(count) => badge(&context, count, (0.88, 0.11, 0.14), true),
            IconState::Warnings(count) => badge(&context, count, (0.96, 0.62, 0.04), false),
            IconState::Quiet | IconState::Unknown => {}
        }
    }
    surface.flush();
    let width = usize::try_from(surface.width()).ok()?;
    let height = usize::try_from(surface.height()).ok()?;
    let stride = usize::try_from(surface.stride()).ok()?;
    let data = surface.data().ok()?;
    // Cairo keeps each pixel as a native-endian, premultiplied 32-bit ARGB number;
    // the tray wants straight ARGB bytes in network order.
    let mut argb = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let (pixels, _) = data[row * stride..row * stride + width * 4].as_chunks::<4>();
        for pixel in pixels {
            let value = u32::from_ne_bytes(*pixel);
            let [alpha, red, green, blue] = value.to_be_bytes();
            let straight = |channel: u8| {
                if alpha == 0 {
                    0
                } else {
                    u8::try_from(u32::from(channel) * 255 / u32::from(alpha)).unwrap_or(255)
                }
            };
            argb.extend_from_slice(&[alpha, straight(red), straight(green), straight(blue)]);
        }
    }
    drop(data);
    Some(ksni::Icon {
        width: i32::try_from(width).ok()?,
        height: i32::try_from(height).ok()?,
        data: argb,
    })
}

fn rounded_square(context: &cairo::Context, x: f64, y: f64, side: f64, radius: f64) {
    context.new_sub_path();
    context.arc(x + side - radius, y + radius, radius, -PI / 2.0, 0.0);
    context.arc(x + side - radius, y + side - radius, radius, 0.0, PI / 2.0);
    context.arc(x + radius, y + side - radius, radius, PI / 2.0, PI);
    context.arc(x + radius, y + radius, radius, PI, 3.0 * PI / 2.0);
    context.close_path();
}

/// The 9: a ring with a tail curling down to the left, as the Mac app draws it.
fn draw_nine(context: &cairo::Context, ink: f64) {
    context.set_source_rgb(ink, ink, ink);
    context.set_line_width(3.0);
    context.set_line_cap(cairo::LineCap::Round);
    context.new_path();
    context.arc(16.0, 12.8, 4.6, 0.0, 2.0 * PI);
    let _ = context.stroke();
    context.move_to(20.6, 12.8);
    context.line_to(20.6, 17.0);
    context.curve_to(20.6, 20.8, 18.4, 23.4, 14.6, 24.0);
    let _ = context.stroke();
}

/// A dot or square in the corner with how many are firing.
fn badge(context: &cairo::Context, count: usize, (r, g, b): (f64, f64, f64), round: bool) {
    let (x, y, side) = (15.0, 15.0, 17.0);
    if round {
        context.arc(x + side / 2.0, y + side / 2.0, side / 2.0, 0.0, 2.0 * PI);
    } else {
        rounded_square(context, x, y, side, 4.0);
    }
    context.set_source_rgb(r, g, b);
    let _ = context.fill();
    let text = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    context.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    context.set_font_size(if text.len() > 1 { 9.5 } else { 12.0 });
    let Ok(extents) = context.text_extents(&text) else {
        return;
    };
    context.set_source_rgb(1.0, 1.0, 1.0);
    context.move_to(
        x + side / 2.0 - extents.width() / 2.0 - extents.x_bearing(),
        y + side / 2.0 - extents.height() / 2.0 - extents.y_bearing(),
    );
    let _ = context.show_text(&text);
}

#[cfg(test)]
mod tests {
    use ksni::Tray as _;

    use super::*;

    /// The colour at `(x, y)` of an icon, as red, green, blue and how opaque.
    fn pixel(icon: &ksni::Icon, x: usize, y: usize) -> (u8, u8, u8, u8) {
        let at = (y * usize::try_from(icon.width).unwrap() + x) * 4;
        let [alpha, red, green, blue] = icon.data[at..at + 4] else {
            unreachable!()
        };
        (red, green, blue, alpha)
    }

    #[test]
    fn the_icon_comes_at_the_sizes_panels_ask_for() {
        let icons = icons(IconState::Quiet);
        let sizes: Vec<(i32, i32)> = icons.iter().map(|icon| (icon.width, icon.height)).collect();
        assert_eq!(sizes, [(22, 22), (32, 32), (48, 48)]);
        for icon in &icons {
            assert_eq!(
                icon.data.len(),
                usize::try_from(icon.width * icon.height * 4).unwrap()
            );
        }
    }

    #[test]
    fn the_icon_says_the_worst_thing_happening() {
        let at_48 = |state| icons(state).pop().unwrap();
        // Cairo may round a channel either way.
        let near = |(r, g, b, a): (u8, u8, u8, u8), (er, eg, eb): (u8, u8, u8)| {
            a == 0xff && r.abs_diff(er) <= 2 && g.abs_diff(eg) <= 2 && b.abs_diff(eb) <= 2
        };
        let green = (0x6f, 0xdc, 0x8c);
        // Outside the rounded square, it's see-through.
        assert_eq!(pixel(&at_48(IconState::Quiet), 0, 0).3, 0);
        // MaxNines' green on its left edge, or grey when the server can't be asked.
        assert!(near(pixel(&at_48(IconState::Quiet), 6, 24), green));
        let (red, gr, blue, _) = pixel(&at_48(IconState::Unknown), 6, 24);
        assert!(
            red.abs_diff(gr) < 12 && gr.abs_diff(blue) < 12,
            "grey, not {red} {gr} {blue}"
        );
        // The badge in the lower right, beside its number: red when critical, amber
        // for warnings only, and none when nothing's firing.
        let badge = |state| pixel(&at_48(state), 43, 30);
        assert!(
            near(badge(IconState::Quiet), green),
            "{:?}",
            badge(IconState::Quiet)
        );
        assert!(
            near(badge(IconState::Critical(3)), (224, 28, 36)),
            "{:?}",
            badge(IconState::Critical(3))
        );
        assert!(
            near(badge(IconState::Warnings(2)), (245, 158, 10)),
            "{:?}",
            badge(IconState::Warnings(2))
        );
    }

    fn labels(tray: &Tray) -> Vec<String> {
        tray.menu()
            .into_iter()
            .map(|item| match item {
                MenuItem::Standard(item) => item.label,
                MenuItem::Separator => "—".into(),
                _ => "?".into(),
            })
            .collect()
    }

    fn tray(firing: usize, connected: bool) -> (Tray, async_channel::Receiver<Command>) {
        let (commands, received) = async_channel::unbounded();
        let tray = Tray {
            headline: "2 critical".into(),
            icons: Vec::new(),
            firing: (1..=firing)
                .map(|n| MenuAlert {
                    label: format!("🔴 disk_{n} is full"),
                    path: format!("/hosts/h{n}"),
                })
                .collect(),
            connected,
            commands,
        };
        (tray, received)
    }

    #[test]
    fn the_menu_lists_whats_firing_and_what_can_be_done() {
        let (signed_in, _) = tray(2, true);
        assert_eq!(
            labels(&signed_in),
            [
                "2 critical",
                "—",
                // An underscore would otherwise mark a keyboard shortcut.
                "🔴 disk__1 is full",
                "🔴 disk__2 is full",
                "—",
                "Show MaxNines Desktop",
                "Open MaxNines in the browser",
                "Refresh",
                "—",
                "Quit",
            ]
        );
        // Not signed in, there's no server to open or ask.
        let (signed_out, _) = tray(0, false);
        assert_eq!(
            labels(&signed_out),
            ["2 critical", "—", "Show MaxNines Desktop", "—", "Quit"]
        );
        // A long list is cut short, with the rest in the window.
        let (busy, _) = tray(12, true);
        let labels = labels(&busy);
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.contains("is full"))
                .count(),
            10
        );
        assert!(labels.contains(&"and 2 more".to_string()), "{labels:?}");
    }

    #[test]
    fn choosing_from_the_menu_tells_the_app() {
        let (mut tray, received) = tray(1, true);
        let mut items = tray.menu();
        let MenuItem::Standard(alert) = items.remove(2) else {
            panic!("not an alert")
        };
        (alert.activate)(&mut tray);
        assert!(matches!(received.try_recv(), Ok(Command::Open(path)) if path == "/hosts/h1"));
        tray.activate(0, 0);
        assert!(matches!(received.try_recv(), Ok(Command::Show)));
    }
}
