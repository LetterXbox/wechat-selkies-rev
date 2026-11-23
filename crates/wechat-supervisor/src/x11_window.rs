use anyhow::Result;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, EventMask, StackMode, Window,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::health::SharedState;

const TRAY_RESERVE_PX: u16 = 28;
const MIN_MAIN_WIDTH: u16 = 520;
const MIN_MAIN_HEIGHT: u16 = 420;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SizeHints {
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

impl SizeHints {
    pub fn is_fixed_size(&self) -> bool {
        let fixed_w =
            matches!((self.min_width, self.max_width), (Some(a), Some(b)) if a > 0 && a == b);
        let fixed_h =
            matches!((self.min_height, self.max_height), (Some(a), Some(b)) if a > 0 && a == b);
        fixed_w && fixed_h
    }
}

pub fn compute_window_geometry(
    viewport_w: u16,
    viewport_h: u16,
    current_w: u16,
    current_h: u16,
    hints: SizeHints,
    reserve_tray: bool,
) -> TargetGeometry {
    let usable_h = if reserve_tray && viewport_h > TRAY_RESERVE_PX + 100 {
        viewport_h - TRAY_RESERVE_PX
    } else {
        viewport_h
    };

    if hints.is_fixed_size() {
        let w = hints
            .min_width
            .unwrap_or(current_w as u32)
            .min(viewport_w as u32);
        let h = hints
            .min_height
            .unwrap_or(current_h as u32)
            .min(usable_h as u32);
        let x = ((viewport_w as i32) - (w as i32)).max(0) / 2;
        let y = ((usable_h as i32) - (h as i32)).max(0) / 2;
        return TargetGeometry {
            x,
            y,
            width: w,
            height: h,
        };
    }

    let mut w = viewport_w as u32;
    let mut h = usable_h as u32;
    if let Some(min_w) = hints.min_width {
        w = w.max(min_w.min(viewport_w as u32));
    }
    if let Some(min_h) = hints.min_height {
        h = h.max(min_h.min(usable_h as u32));
    }
    if let Some(max_w) = hints.max_width.filter(|&v| v > 0) {
        w = w.min(max_w);
    }
    if let Some(max_h) = hints.max_height.filter(|&v| v > 0) {
        h = h.min(max_h);
    }

    TargetGeometry {
        x: ((viewport_w as i32) - (w as i32)).max(0) / 2,
        y: ((usable_h as i32) - (h as i32)).max(0) / 2,
        width: w.max(1),
        height: h.max(1),
    }
}

pub fn spawn_watcher(state: Arc<SharedState>, show_tray: bool) -> Result<JoinHandle<()>> {
    let handle = std::thread::Builder::new()
        .name("x11-autofit".to_string())
        .spawn(move || watcher_loop(state, show_tray))?;
    Ok(handle)
}

pub fn fit_once(show_tray: bool) -> Result<()> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;
    let atoms = Atoms::intern(&conn)?;
    let _ = fit_wechat_windows(&conn, root, &atoms, show_tray, true);
    Ok(())
}

fn watcher_loop(state: Arc<SharedState>, show_tray: bool) {
    while !state.shutdown.load(Ordering::Relaxed) {
        match x11rb::connect(None) {
            Ok((conn, screen_num)) => {
                if let Err(err) = run_connected_loop(&conn, screen_num, &state, show_tray) {
                    eprintln!("[wechat-x11] Disconnected from X11: {err}");
                }
                state.set_display_state(false, None);
            }
            Err(_) => {
                state.set_display_state(false, None);
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run_connected_loop(
    conn: &RustConnection,
    screen_num: usize,
    state: &SharedState,
    show_tray: bool,
) -> Result<()> {
    let root = conn.setup().roots[screen_num].root;
    let atoms = Atoms::intern(conn)?;

    let _ = conn.randr_select_input(root, NotifyMask::SCREEN_CHANGE);
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new()
            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::SUBSTRUCTURE_NOTIFY),
    )?;
    conn.flush()?;

    let mut last_viewport: Option<(u16, u16)> = None;
    let mut last_fit = Instant::now() - Duration::from_secs(10);

    while !state.shutdown.load(Ordering::Relaxed) {
        let mut need_fit = state.fit_requested.swap(false, Ordering::SeqCst);
        let mut raise_window = need_fit;

        while let Some(event) = conn.poll_for_event()? {
            match event {
                Event::RandrScreenChangeNotify(_) | Event::ConfigureNotify(_) => {
                    need_fit = true;
                }
                Event::PropertyNotify(ev) if ev.atom == atoms.net_client_list => {
                    need_fit = true;
                }
                _ => {}
            }
        }

        if let Ok(geom) = conn.get_geometry(root)?.reply() {
            let vp = (geom.width, geom.height);
            state.set_display_state(true, Some(vp));
            if last_viewport != Some(vp) {
                last_viewport = Some(vp);
                need_fit = true;
            }
        }

        if need_fit && (raise_window || last_fit.elapsed() >= Duration::from_millis(350)) {
            let _ = fit_wechat_windows(conn, root, &atoms, show_tray, raise_window);
            last_fit = Instant::now();
            raise_window = false;
        }
        let _ = raise_window;

        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

struct Atoms {
    net_client_list: Atom,
    net_wm_window_type: Atom,
    net_wm_window_type_normal: Atom,
    net_active_window: Atom,
    net_wm_state: Atom,
    net_wm_state_maximized_horz: Atom,
    net_wm_state_maximized_vert: Atom,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self> {
        let intern =
            |name: &[u8]| -> Result<Atom> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
        Ok(Self {
            net_client_list: intern(b"_NET_CLIENT_LIST")?,
            net_wm_window_type: intern(b"_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_normal: intern(b"_NET_WM_WINDOW_TYPE_NORMAL")?,
            net_active_window: intern(b"_NET_ACTIVE_WINDOW")?,
            net_wm_state: intern(b"_NET_WM_STATE")?,
            net_wm_state_maximized_horz: intern(b"_NET_WM_STATE_MAXIMIZED_HORZ")?,
            net_wm_state_maximized_vert: intern(b"_NET_WM_STATE_MAXIMIZED_VERT")?,
        })
    }
}

fn fit_wechat_windows(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
    show_tray: bool,
    raise: bool,
) -> Result<()> {
    let root_geom = conn.get_geometry(root)?.reply()?;
    let prop = conn
        .get_property(
            false,
            root,
            atoms.net_client_list,
            AtomEnum::WINDOW,
            0,
            1024,
        )?
        .reply()?;
    let Some(windows) = prop.value32() else {
        return Ok(());
    };

    for win in windows {
        if !is_wechat_normal_window(conn, win, atoms)? {
            continue;
        }
        let Ok(geom) = conn.get_geometry(win)?.reply() else {
            continue;
        };
        let hints = read_size_hints(conn, win).unwrap_or_default();

        if !hints.is_fixed_size()
            && geom.width < MIN_MAIN_WIDTH
            && geom.height < MIN_MAIN_HEIGHT
            && hints.min_width.unwrap_or(0) < MIN_MAIN_WIDTH as u32
        {
            continue;
        }

        let target = compute_window_geometry(
            root_geom.width,
            root_geom.height,
            geom.width,
            geom.height,
            hints,
            show_tray,
        );

        clear_maximized_state(conn, root, win, atoms);
        let mut aux = ConfigureWindowAux::new()
            .x(target.x)
            .y(target.y)
            .width(target.width)
            .height(target.height);
        if raise {
            let _ = conn.map_window(win);
            aux = aux.stack_mode(StackMode::ABOVE);
        }
        let _ = conn.configure_window(win, &aux);
        if raise {
            activate_window(conn, root, win, atoms);
        }
    }
    conn.flush()?;
    Ok(())
}

fn is_wechat_normal_window(conn: &RustConnection, win: Window, atoms: &Atoms) -> Result<bool> {
    let class_reply = conn
        .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?
        .reply()?;
    let class_str = String::from_utf8_lossy(&class_reply.value).to_ascii_lowercase();
    if !class_str.contains("wechat") {
        return Ok(false);
    }

    let type_reply = conn
        .get_property(false, win, atoms.net_wm_window_type, AtomEnum::ATOM, 0, 16)?
        .reply()?;
    if let Some(mut types) = type_reply.value32() {
        let mut has_any = false;
        let mut has_normal = false;
        for t in &mut types {
            has_any = true;
            if t == atoms.net_wm_window_type_normal {
                has_normal = true;
            }
        }
        if has_any && !has_normal {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_size_hints(conn: &RustConnection, win: Window) -> Result<SizeHints> {
    let reply = conn
        .get_property(
            false,
            win,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            0,
            18,
        )?
        .reply()?;
    let vals: Vec<u32> = reply.value32().map(|it| it.collect()).unwrap_or_default();
    if vals.len() < 9 {
        return Ok(SizeHints::default());
    }
    let flags = vals[0];
    let has_min = (flags & (1 << 4)) != 0;
    let has_max = (flags & (1 << 5)) != 0;
    Ok(SizeHints {
        min_width: has_min.then_some(vals[5]),
        min_height: has_min.then_some(vals[6]),
        max_width: has_max.then_some(vals[7]),
        max_height: has_max.then_some(vals[8]),
    })
}

fn clear_maximized_state(conn: &RustConnection, root: Window, win: Window, atoms: &Atoms) {
    let ev = ClientMessageEvent::new(
        32,
        win,
        atoms.net_wm_state,
        [
            0,
            atoms.net_wm_state_maximized_horz,
            atoms.net_wm_state_maximized_vert,
            1,
            0,
        ],
    );
    let _ = conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    );
}

fn activate_window(conn: &RustConnection, root: Window, win: Window, atoms: &Atoms) {
    let ev = ClientMessageEvent::new(32, win, atoms.net_active_window, [1, 0, 0, 0, 0]);
    let _ = conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_resizable_main_window_to_viewport_with_tray_margin() {
        let geom = compute_window_geometry(1920, 1080, 900, 700, SizeHints::default(), true);
        assert_eq!(
            geom,
            TargetGeometry {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080 - TRAY_RESERVE_PX as u32,
            }
        );
    }

    #[test]
    fn centers_fixed_size_login_dialog_without_stretching() {
        let hints = SizeHints {
            min_width: Some(380),
            min_height: Some(500),
            max_width: Some(380),
            max_height: Some(500),
        };
        let geom = compute_window_geometry(1280, 800, 380, 500, hints, false);
        assert_eq!(
            geom,
            TargetGeometry {
                x: 450,
                y: 150,
                width: 380,
                height: 500,
            }
        );
    }
}
