//! Displays: asked of the alpheus daemon, which owns the compositor's output
//! management (displays protocol spec), over the launcher's owner-only socket.
//! Never the compositor directly, never a command.
//!
//! Never:
//! - call the service behind the tool: the tool is the interface, and a missing one leaves the row
//!   or the tab empty rather than failing;
//! - run anything in a test: the fake executor answers (AGENTS.md).
//!
//! See: docs/features/control-center.md, docs/install.md (the tools), docs/compositors.md (what
//! `wm-set` and `shell-ctl` stand for).

use crate::displays::Step;
use crate::displays::wire::{Snapshot, decode_snapshot, encode_desired};
use crate::launcher_ipc::{Reply, Request, exchange_waiting};
use std::io::ErrorKind;
use std::path::PathBuf;

pub const NOT_RUNNING: &str = "the alpheus daemon is not running";
pub const TOO_OLD: &str = "the daemon is too old for displays";
const UNEXPECTED: &str = "unexpected answer from the alpheus daemon";

pub trait Displays: Send + Sync {
    /// The daemon's heads and its trial, from memory.
    fn snapshot(&self) -> Result<Snapshot, String>;
    /// Tries `steps` (the outputs that change): answered once the compositor has, a 15 s trial
    /// then open in the daemon.
    fn try_(&self, steps: &[Step]) -> Result<(), String>;
    /// Keeps the trial that is open.
    fn keep(&self) -> Result<(), String>;
    /// Reverts the trial that is open.
    fn revert(&self) -> Result<(), String>;
}

/// The client of the daemon's `outputs` verbs. Every error is a sentence the tab shows as is.
pub struct DaemonDisplays {
    socket: PathBuf,
}

impl DaemonDisplays {
    pub fn new(socket: PathBuf) -> Self {
        DaemonDisplays { socket }
    }

    fn ask(&self, request: &Request) -> Result<Reply, String> {
        let reply = exchange_waiting(&self.socket, request, request.reply_timeout()).map_err(|e| match e.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => NOT_RUNNING.to_string(),
            _ => e.to_string(),
        })?;
        match reply {
            Reply::Error(e) if request.refused_as_unknown_displays_verb(&e) => Err(TOO_OLD.into()),
            Reply::Error(e) => Err(e),
            r => Ok(r),
        }
    }

    fn done(&self, request: &Request) -> Result<(), String> {
        match self.ask(request)? {
            Reply::Done => Ok(()),
            _ => Err(UNEXPECTED.into()),
        }
    }
}

impl Displays for DaemonDisplays {
    fn snapshot(&self) -> Result<Snapshot, String> {
        match self.ask(&Request::Outputs)? {
            Reply::Outputs(body) => decode_snapshot(&body).map_err(|e| format!("unreadable displays from the daemon: {e}")),
            _ => Err(UNEXPECTED.into()),
        }
    }

    fn try_(&self, steps: &[Step]) -> Result<(), String> {
        self.done(&Request::OutputsTry(encode_desired(steps)))
    }

    fn keep(&self) -> Result<(), String> {
        self.done(&Request::OutputsKeep)
    }

    fn revert(&self) -> Result<(), String> {
        self.done(&Request::OutputsRevert)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::testing::*;
    use crate::displays::wire::{Status, TrialState, decode_desired, encode_snapshot};
    use crate::launcher_ipc::{RequestDecoder, encode_reply};
    use crate::temp_path::TempPath;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::thread::JoinHandle;

    /// A daemon of one connection on a socket in the test's own directory: it reads one request,
    /// answers `reply` and hands the request back.
    fn serve(dir: &Path, reply: Reply) -> (DaemonDisplays, JoinHandle<Request>) {
        let socket = dir.join("alpheus-launcher.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        let h = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut decoder = RequestDecoder::new();
            let mut chunk = [0u8; 4096];
            let request = loop {
                let n = conn.read(&mut chunk).unwrap();
                assert!(n > 0, "the client hung up before a whole request");
                if let Some(r) = decoder.feed(&chunk[..n]).unwrap() {
                    break r;
                }
            };
            conn.write_all(&encode_reply(&reply)).unwrap();
            request
        });
        (DaemonDisplays::new(socket), h)
    }

    fn sample() -> Snapshot {
        Snapshot { status: Status::Ready, outputs: machine(), trial: TrialState::Open { seconds_left: 9 } }
    }

    #[test]
    fn the_snapshot_is_read_from_the_daemon() {
        let dir = TempPath::dir("displays-snapshot");
        let (d, h) = serve(&dir, Reply::Outputs(encode_snapshot(&sample())));
        assert_eq!(d.snapshot(), Ok(sample()));
        assert_eq!(h.join().unwrap(), Request::Outputs);
    }

    #[test]
    fn a_try_sends_the_steps_and_keep_and_revert_their_verbs() {
        let dir = TempPath::dir("displays-verbs");
        let steps =
            vec![Step { output: "eDP-1".into(), active: None }, Step { output: "DVI-I-1".into(), active: machine()[1].active.clone() }];
        let (d, h) = serve(&dir, Reply::Done);
        assert_eq!(d.try_(&steps), Ok(()));
        let Request::OutputsTry(body) = h.join().unwrap() else { panic!("not a try") };
        assert_eq!(decode_desired(&body), Ok(steps));
        let (d, h) = serve(&dir, Reply::Done);
        assert_eq!(d.keep(), Ok(()));
        assert_eq!(h.join().unwrap(), Request::OutputsKeep);
        let (d, h) = serve(&dir, Reply::Done);
        assert_eq!(d.revert(), Ok(()));
        assert_eq!(h.join().unwrap(), Request::OutputsRevert);
    }

    #[test]
    fn the_daemons_words_are_shown_as_they_are() {
        let dir = TempPath::dir("displays-error");
        for e in ["displays are off in bar.toml", "the compositor has no output management", "the compositor refused this configuration"] {
            let (d, h) = serve(&dir, Reply::Error(e.into()));
            assert_eq!(d.try_(&[]), Err(e.to_string()));
            h.join().unwrap();
        }
    }

    #[test]
    fn a_daemon_older_than_the_verbs_is_too_old() {
        let dir = TempPath::dir("displays-too-old");
        let (d, h) = serve(&dir, Reply::Error("malformed request: unknown request \"outputs\"".into()));
        assert_eq!(d.snapshot(), Err(TOO_OLD.to_string()));
        h.join().unwrap();
    }

    #[test]
    fn no_socket_or_no_listener_is_a_daemon_not_running() {
        let dir = TempPath::dir("displays-absent");
        let d = DaemonDisplays::new(dir.join("alpheus-launcher.sock"));
        assert_eq!(d.snapshot(), Err(NOT_RUNNING.to_string()));
        // A socket file left behind by a daemon that died: refused.
        drop(UnixListener::bind(dir.join("alpheus-launcher.sock")).unwrap());
        assert_eq!(d.revert(), Err(NOT_RUNNING.to_string()));
    }

    #[test]
    fn an_answer_of_the_wrong_kind_or_unreadable_is_an_error() {
        let dir = TempPath::dir("displays-unexpected");
        let (d, h) = serve(&dir, Reply::Done);
        assert_eq!(d.snapshot(), Err(UNEXPECTED.to_string()));
        h.join().unwrap();
        let (d, h) = serve(&dir, Reply::Outputs("status maybe\ntrial none\n".into()));
        assert_eq!(d.snapshot(), Err("unreadable displays from the daemon: unknown status".to_string()));
        h.join().unwrap();
        let (d, h) = serve(&dir, Reply::Cancelled);
        assert_eq!(d.keep(), Err(UNEXPECTED.to_string()));
        h.join().unwrap();
    }
}
