//! Scripted input for testing, from `SLERP_INPUT`: key presses,
//! drags, clicks and scrolls fed to egui as if they came from the window
//! system, so tests drive the same code paths a user does without sending
//! real input to the desktop.
//!
//! Steps are separated by spaces and run one per frame once the file has
//! loaded:
//!
//! - a key with optional modifiers: `Tab`, `F`, `1`, `ctrl+shift+C`
//! - `hold:W:30`: hold a key for 30 frames (for flying)
//! - `drag:120,-40` or `drag:120,-40:right`: drag across the view's
//!   centre by that many points, with the primary, middle or right button
//! - `dragat:1230,175:-60,0`: drag from a point of the window by that many
//!   points with the primary button, for panel sliders
//! - `click:0.5,0.4` and `dblclick:0.5,0.4`: at a fraction of the view's
//!   width and height; `clickat:1104,131` at a point of the window, for
//!   panel controls
//! - `move:0.5,0.4`: point there, for hover tips; `moveat:90,281` at a
//!   point of the window, for hovering panel rows
//! - `scroll:3`: wheel notches away from the user; `scrollat:1104,131:-1`
//!   at a point of the window, for panel controls
//! - `press:back` and `press:forward`: the mouse's back and forward
//!   buttons, over the view's centre, or `press:back:1300,400` at a point
//!   of the window; `press:right:1300,400` is a right click, for context
//!   menus
//! - `wait:10`: do nothing for 10 frames; `sleep:1500`: for 1.5 seconds,
//!   for waiting on something outside the window, such as a file being
//!   rewritten
//! - `shot:/tmp/a.png`: save the window as it is now, and carry on once
//!   the picture has arrived (a window can take a while to deliver one);
//!   the mode and file shown go in `/tmp/a.png.txt`, and for 3D views the
//!   view state in `/tmp/a.png.jsonc`

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Key(Modifiers, Key),
    Hold(Key, u32),
    Drag(Vec2, PointerButton),
    /// A primary-button drag that starts at a point of the window.
    DragAt(Pos2, Vec2),
    Click(Vec2, bool),
    ClickAt(Pos2),
    Move(Vec2),
    /// The pointer to a point of the window.
    MoveAt(Pos2),
    Scroll(f32),
    /// Wheel notches with the pointer at a point of the window.
    ScrollAt(Pos2, f32),
    /// A press and release of a button without moving the pointer: the
    /// mouse's back, forward or right button, at a window point or the
    /// view's centre.
    Press(PointerButton, Option<Pos2>),
    Wait(u32),
    Sleep(Duration),
    Shot(PathBuf),
}

/// Frames a drag is spread over.
const DRAG_FRAMES: u32 = 6;

pub struct Script {
    steps: VecDeque<Step>,
    /// A step spread over several frames and how far it got.
    current: Option<(Step, u32)>,
    held: Vec<(Key, u32)>,
    /// When the `sleep` step in progress started.
    sleeping: Option<Instant>,
    /// A screenshot a `shot` step asked for, for the viewer to take.
    shot: Option<PathBuf>,
    /// Waiting for that screenshot before the next step.
    waiting: bool,
}

impl Script {
    pub fn from_env() -> Option<Script> {
        let text = crate::env::var("SLERP_INPUT")?;
        match Script::parse(&text) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("slerp view: SLERP_INPUT: {e}");
                None
            }
        }
    }

    pub fn parse(text: &str) -> Result<Script, String> {
        let mut steps = VecDeque::new();
        for token in text.split_whitespace() {
            steps.push_back(parse_step(token).ok_or_else(|| format!("can't read {token:?}"))?);
        }
        Ok(Script { steps, current: None, held: Vec::new(), sleeping: None, shot: None, waiting: false })
    }

    pub fn is_done(&self) -> bool {
        self.steps.is_empty() && self.current.is_none() && self.held.is_empty() && !self.waiting
    }

    /// The screenshot a `shot` step asked for this frame.  The script
    /// waits until [`Script::shot_taken`].
    pub fn take_shot(&mut self) -> Option<PathBuf> {
        self.shot.take()
    }

    pub fn shot_taken(&mut self) {
        self.waiting = false;
    }

    /// The events for this frame, in a view at `rect` (points).
    pub fn events(&mut self, rect: Rect) -> Vec<Event> {
        let mut out = Vec::new();
        let at = |f: Vec2| pos2(rect.left() + rect.width() * f.x, rect.top() + rect.height() * f.y);
        let button_event =
            |pos: Pos2, button, pressed| Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE };
        // 1. Keys being held.
        self.held.retain_mut(|(key, frames)| {
            if *frames == 0 {
                out.push(key_event(*key, false, Modifiers::NONE));
                false
            } else {
                *frames -= 1;
                true
            }
        });
        // 2. The step in progress, or the next one, unless a screenshot is
        //    on its way.
        if self.waiting {
            return out;
        }
        let (step, frame) = match self.current.take() {
            Some(c) => c,
            None => match self.steps.pop_front() {
                Some(s) => (s, 0),
                None => return out,
            },
        };
        let mut more = false;
        match &step {
            Step::Key(mods, key) => {
                out.push(key_event(*key, true, *mods));
                out.push(key_event(*key, false, *mods));
            }
            Step::Hold(key, frames) => {
                out.push(key_event(*key, true, Modifiers::NONE));
                self.held.push((*key, *frames));
            }
            Step::Drag(..) | Step::DragAt(..) => {
                let (start, delta, button) = match &step {
                    Step::Drag(delta, button) => (rect.center(), *delta, *button),
                    Step::DragAt(start, delta) => (*start, *delta, PointerButton::Primary),
                    _ => unreachable!(),
                };
                if frame == 0 {
                    out.push(Event::PointerMoved(start));
                    out.push(button_event(start, button, true));
                    more = true;
                } else if frame <= DRAG_FRAMES {
                    out.push(Event::PointerMoved(start + delta * (frame as f32 / DRAG_FRAMES as f32)));
                    more = true;
                } else {
                    out.push(button_event(start + delta, button, false));
                }
            }
            Step::Click(f, double) => {
                let p = at(*f);
                out.push(Event::PointerMoved(p));
                let presses = if *double { 2 } else { 1 };
                for _ in 0..presses {
                    out.push(button_event(p, PointerButton::Primary, true));
                    out.push(button_event(p, PointerButton::Primary, false));
                }
            }
            Step::ClickAt(p) => {
                out.push(Event::PointerMoved(*p));
                out.push(button_event(*p, PointerButton::Primary, true));
                out.push(button_event(*p, PointerButton::Primary, false));
            }
            Step::Move(f) => out.push(Event::PointerMoved(at(*f))),
            Step::MoveAt(p) => out.push(Event::PointerMoved(*p)),
            Step::Scroll(n) => out.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(0.0, *n),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            }),
            Step::ScrollAt(p, n) => {
                out.push(Event::PointerMoved(*p));
                out.push(Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: vec2(0.0, *n),
                    phase: egui::TouchPhase::Move,
                    modifiers: Modifiers::NONE,
                });
            }
            Step::Press(button, at) => {
                let p = at.unwrap_or(rect.center());
                out.push(button_event(p, *button, true));
                out.push(button_event(p, *button, false));
            }
            Step::Wait(n) => more = frame + 1 < *n,
            Step::Sleep(d) => {
                let started = *self.sleeping.get_or_insert_with(Instant::now);
                more = started.elapsed() < *d;
                if !more {
                    self.sleeping = None;
                }
            }
            Step::Shot(path) => {
                self.shot = Some(path.clone());
                self.waiting = true;
            }
        }
        if more {
            self.current = Some((step, frame + 1));
        }
        out
    }
}

fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }
}

fn parse_step(token: &str) -> Option<Step> {
    let pair = |s: &str| -> Option<Vec2> {
        let (a, b) = s.split_once(',')?;
        Some(vec2(a.trim().parse().ok()?, b.trim().parse().ok()?))
    };
    if let Some((kind, rest)) = token.split_once(':') {
        return Some(match kind {
            "hold" => {
                let (key, frames) = rest.split_once(':')?;
                Step::Hold(Key::from_name(key)?, frames.parse().ok()?)
            }
            "drag" => {
                let (delta, button) = rest.split_once(':').unwrap_or((rest, "left"));
                let button = match button {
                    "left" | "primary" => PointerButton::Primary,
                    "middle" => PointerButton::Middle,
                    "right" | "secondary" => PointerButton::Secondary,
                    _ => return None,
                };
                Step::Drag(pair(delta)?, button)
            }
            "dragat" => {
                let (at, delta) = rest.split_once(':')?;
                Step::DragAt(pair(at)?.to_pos2(), pair(delta)?)
            }
            "click" => Step::Click(pair(rest)?, false),
            "dblclick" => Step::Click(pair(rest)?, true),
            "clickat" => Step::ClickAt(pair(rest)?.to_pos2()),
            "move" => Step::Move(pair(rest)?),
            "moveat" => Step::MoveAt(pair(rest)?.to_pos2()),
            "scroll" => Step::Scroll(rest.parse().ok()?),
            "scrollat" => {
                let (at, n) = rest.rsplit_once(':')?;
                Step::ScrollAt(pair(at)?.to_pos2(), n.parse().ok()?)
            }
            "press" => {
                let (button, at) = rest.split_once(':').map_or((rest, None), |(b, at)| (b, Some(at)));
                let button = match button {
                    "back" => PointerButton::Extra1,
                    "forward" => PointerButton::Extra2,
                    "right" => PointerButton::Secondary,
                    _ => return None,
                };
                let at = match at {
                    Some(a) => Some(pair(a)?.to_pos2()),
                    None => None,
                };
                Step::Press(button, at)
            }
            "wait" => Step::Wait(rest.parse().ok()?),
            "sleep" => Step::Sleep(Duration::from_millis(rest.parse().ok()?)),
            "shot" if !rest.is_empty() => Step::Shot(PathBuf::from(rest)),
            _ => return None,
        });
    }
    let mut mods = Modifiers::NONE;
    let mut parts: Vec<&str> = token.split('+').collect();
    let key = parts.pop()?;
    for m in parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::COMMAND,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" => mods |= Modifiers::ALT,
            _ => return None,
        }
    }
    // Plain letters arrive as egui's key names, which are upper case.
    let key = if key.len() == 1 { key.to_ascii_uppercase() } else { key.to_string() };
    Some(Step::Key(mods, Key::from_name(&key)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_step() {
        let s = Script::parse("Tab ctrl+shift+c hold:W:5 drag:10,-4:right click:0.5,0.25 dblclick:0.1,0.1 move:0.2,0.3 scroll:-2 wait:3 1 press:back press:forward:1300,400 sleep:250 shot:/tmp/a.png scrollat:10,20:-1")
            .unwrap();
        assert_eq!(s.steps.len(), 15);
        assert_eq!(s.steps[14], Step::ScrollAt(pos2(10.0, 20.0), -1.0));
        assert_eq!(s.steps[10], Step::Press(PointerButton::Extra1, None));
        assert_eq!(s.steps[11], Step::Press(PointerButton::Extra2, Some(pos2(1300.0, 400.0))));
        assert_eq!(s.steps[12], Step::Sleep(Duration::from_millis(250)));
        assert_eq!(s.steps[13], Step::Shot(PathBuf::from("/tmp/a.png")));
        assert!(Script::parse("press:middle").is_err());
        assert_eq!(s.steps[1], Step::Key(Modifiers::COMMAND | Modifiers::SHIFT, Key::C));
        assert_eq!(s.steps[3], Step::Drag(vec2(10.0, -4.0), PointerButton::Secondary));
        assert_eq!(s.steps[9], Step::Key(Modifiers::NONE, Key::Num1));
        assert!(Script::parse("hold:W").is_err());
        assert!(Script::parse("nonsense:1").is_err());
    }

    #[test]
    fn drags_can_start_at_a_window_point() {
        let s = Script::parse("dragat:1203,175:60,-2").unwrap();
        assert_eq!(s.steps[0], Step::DragAt(pos2(1203.0, 175.0), vec2(60.0, -2.0)));
        assert!(Script::parse("dragat:1203,175").is_err());
    }

    #[test]
    fn drags_press_move_and_release() {
        let mut s = Script::parse("drag:60,0").unwrap();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let mut all = Vec::new();
        while !s.is_done() {
            all.extend(s.events(rect));
        }
        let presses = all.iter().filter(|e| matches!(e, Event::PointerButton { pressed: true, .. })).count();
        let releases: Vec<Pos2> = all
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { pressed: false, pos, .. } => Some(*pos),
                _ => None,
            })
            .collect();
        assert_eq!(presses, 1);
        assert_eq!(releases, vec![pos2(110.0, 50.0)]);
    }

    #[test]
    fn mouse_buttons_press_in_place_and_sleep_waits() {
        let mut s = Script::parse("press:back sleep:60 shot:/tmp/x.png").unwrap();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0));
        let events = s.events(rect);
        let presses: Vec<(PointerButton, bool, Pos2)> = events
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { button, pressed, pos, .. } => Some((*button, *pressed, *pos)),
                _ => None,
            })
            .collect();
        assert_eq!(
            presses,
            vec![(PointerButton::Extra1, true, pos2(50.0, 25.0)), (PointerButton::Extra1, false, pos2(50.0, 25.0))]
        );
        assert!(!events.iter().any(|e| matches!(e, Event::PointerMoved(_))), "the pointer stays put");
        let start = Instant::now();
        while s.take_shot().is_none() {
            s.events(rect);
        }
        assert!(start.elapsed() >= Duration::from_millis(60));
        assert!(!s.is_done(), "waits for the picture");
        s.shot_taken();
        assert!(s.is_done());
    }

    #[test]
    fn held_keys_are_released() {
        let mut s = Script::parse("hold:W:2").unwrap();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0));
        let mut frames = 0;
        let mut released = false;
        while !s.is_done() {
            released |= s.events(rect).iter().any(|e| matches!(e, Event::Key { pressed: false, .. }));
            frames += 1;
        }
        assert!(released);
        assert_eq!(frames, 4);
    }
}
