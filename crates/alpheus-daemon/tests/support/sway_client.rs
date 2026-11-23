//! Test-only tools for THIS test's isolated headless sway (its own runtime directory, never the user's
//! session): `Sway::ipc_raw` sends one raw i3-ipc message on this sway's own IPC socket and
//! returns the reply's payload (the Sway fixtures are captured with it); `TestWindow` maps a plain
//! xdg toplevel (the headless sway has no client of its own, and the dev shell no terminal).
use super::Sway;
use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle, delegate_noop};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

impl Sway {
    /// One raw i3-ipc message (`kind`, `payload`) on THIS sway's own IPC socket; the reply's payload.
    /// Test code only: the daemon's own client is `sources::sway`.
    pub fn ipc_raw(&self, kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut s = UnixStream::connect(&self.ipc).unwrap();
        let mut m = b"i3-ipc".to_vec();
        m.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        m.extend_from_slice(&kind.to_le_bytes());
        m.extend_from_slice(payload);
        s.write_all(&m).unwrap();
        let mut header = [0u8; 14];
        s.read_exact(&mut header).unwrap();
        assert_eq!(&header[..6], b"i3-ipc");
        assert_eq!(u32::from_le_bytes(header[10..14].try_into().unwrap()), kind, "the reply's type");
        let mut body = vec![0u8; u32::from_le_bytes(header[6..10].try_into().unwrap()) as usize];
        s.read_exact(&mut body).unwrap();
        body
    }
}

/// The `id` of the tree node whose `app_id` is `app_id` (a GET_TREE reply), if any.
pub fn con_id(tree: &[u8], app_id: &str) -> Option<u64> {
    fn find(v: &serde_json::Value, app_id: &str) -> Option<u64> {
        if v.get("app_id").and_then(|a| a.as_str()) == Some(app_id) {
            return v.get("id").and_then(|i| i.as_u64());
        }
        ["nodes", "floating_nodes"].iter().filter_map(|k| v.get(*k).and_then(|n| n.as_array())).flatten().find_map(|c| find(c, app_id))
    }
    find(&serde_json::from_slice(tree).ok()?, app_id)
}

/// A mapped xdg toplevel on THIS test's headless sway, a 64×64 black buffer, its `app_id` and
/// `title` set before the first commit. A thread answers pings and ends when sway asks the window
/// to close (`[app_id=…] kill`) or goes away; the connection closes with it and the window is gone.
pub struct TestWindow {
    closed: Arc<AtomicBool>,
}

struct WindowState {
    surface: wl_surface::WlSurface,
    buffer: wl_buffer::WlBuffer,
    configured: bool,
    closed: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WindowState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for WindowState {
    fn event(_: &mut Self, base: &xdg_wm_base::XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for WindowState {
    fn event(state: &mut Self, xdg: &xdg_surface::XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            state.surface.attach(Some(&state.buffer), 0, 0);
            state.surface.commit();
            state.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for WindowState {
    fn event(state: &mut Self, _: &xdg_toplevel::XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_toplevel::Event::Close = event {
            state.closed = true;
        }
    }
}

delegate_noop!(WindowState: ignore wl_compositor::WlCompositor);
delegate_noop!(WindowState: ignore wl_surface::WlSurface);
delegate_noop!(WindowState: ignore wl_shm::WlShm);
delegate_noop!(WindowState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(WindowState: ignore wl_buffer::WlBuffer);

impl TestWindow {
    pub fn open(sway: &Sway, app_id: &str, title: &str) -> TestWindow {
        let stream = UnixStream::connect(sway.runtime.join(&sway.wayland)).unwrap();
        let connection = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<WindowState>(&connection).unwrap();
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 1..=4, ()).expect("wl_compositor");
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
        let base: xdg_wm_base::XdgWmBase = globals.bind(&qh, 1..=2, ()).expect("xdg_wm_base");
        let surface = compositor.create_surface(&qh, ());
        let xdg = base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        toplevel.set_title(title.into());
        surface.commit();
        let (width, height) = (64, 64);
        let file = tempfile_in(&sway.runtime, app_id);
        file.set_len((width * height * 4) as u64).unwrap();
        let pool = shm.create_pool(file.as_fd(), width * height * 4, &qh, ());
        let buffer = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Xrgb8888, &qh, ());
        let mut state = WindowState { surface, buffer, configured: false, closed: false };
        while !state.configured {
            queue.blocking_dispatch(&mut state).unwrap();
        }
        queue.roundtrip(&mut state).unwrap(); // the commit with the buffer reached sway: mapped
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        std::thread::spawn(move || {
            let _alive = (connection, compositor, shm, base, xdg, toplevel, pool, file);
            while !state.closed && queue.blocking_dispatch(&mut state).is_ok() {}
            flag.store(true, Ordering::SeqCst);
        });
        TestWindow { closed }
    }

    /// The window was closed (sway asked) or its compositor went away.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// The shm file behind a window's buffer, in THIS sway's runtime directory (removed with it).
fn tempfile_in(runtime: &std::path::Path, app_id: &str) -> std::fs::File {
    let path = runtime.join(format!("window-{app_id}.shm"));
    std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path).unwrap()
}
